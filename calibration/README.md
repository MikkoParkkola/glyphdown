# Calibration dialects

`snapshot.json` is the published **calibration snapshot** — one *dialect* per
Claude model. A dialect is a measured `tokens_per_char` rate (with a confidence
in `[0, 1]`) the codec uses to estimate token cost for that model.

See [../METHODOLOGY.md](../METHODOLOGY.md) for how dialects are produced and what
is and is not published.

## Format

```jsonc
{
  "schema": "ultracos-calib-v1",
  "snapshot_version": "YYYY.MM.DD",
  "models": {
    "<model-id>": {
      "tokens_per_char": { "_default": 0.0 },   // measured rate for this model
      "confidence":      { "_default": 0.0 }     // [0,1]; below the gate -> fallback
    }
  },
  "fallback_tokens_per_char": 0.25,              // classic 4-characters-per-token
  "min_confidence_to_use": 0.8                   // dialects under this gate use fallback
}
```

Every value is fitted from real, model-billed token counts. Different models can
have different rates (that is why each gets its own dialect), and a model's rate
can change with a model update — so the snapshot is refreshed and republished as
the models change.

## Status (honest)

- The dialects here are real, fitted from measured token counts.
- Wiring the codec to read this file — including how the active model is
  identified at codec time — is the next step and is tracked. Until it lands, the
  codec uses the `0.25` fallback. This file is the artifact that step consumes.
- The per-model rate reflects measured real usage (tokenizer plus typical content
  mix). It is not, on its own, a claim about a model's internal tokenizer.
