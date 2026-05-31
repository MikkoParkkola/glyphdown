# ultracos-set-level

Set the UltraCoS skill-level intensity flag.

## Usage

```
/ultracos-set-level <level>
```

## Levels

- **lite** — minimal vocabulary (core symbols only)
- **full** — standard vocabulary (default; full ASCII dialect)
- **ultra** — maximal compression (3-tok symbols banned, abbreviations strict)
- **off** — disable skill injection at SessionStart

## Examples

```
/ultracos-set-level lite
/ultracos-set-level full
/ultracos-set-level ultra
/ultracos-set-level off
```

## How it works

The level is persisted to `~/.ultracos/active-level`.
The SessionStart hook reads this file and filters the single-source
register-variants table in SKILL.md by level before injecting into the
session context. Changing the level takes effect on the next session.

## Default

If the flag file does not exist, the default level is **full**.
