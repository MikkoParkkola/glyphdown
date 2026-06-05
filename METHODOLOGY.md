# How Glyphdown calibration works

This document is deliberately transparent about the **approach**, so you can
trust the published result and judge it. It does not contain the data, the
pipeline, or the exact fitting procedure that produce the numbers — those are
what make a published snapshot a result you can use but not regenerate.

## The problem

To decide whether compacting a tool result is worth it, the codec needs to know
how many tokens that text costs. A character is not a fixed number of tokens:
the rate depends on the model's tokenizer and on the kind of content. A single
hardcoded rate (the common "4 characters per token") is therefore wrong by a
model- and content-dependent amount.

## What a "dialect" is

For each Claude model we use, we publish a **dialect**: a measured
`tokens_per_char` rate (with a confidence) that the codec uses for that model.
Different models can have different rates, so each gets its own dialect.

Each value is **fitted from real, model-billed token counts** — the token counts
the model itself reports for actual usage — rather than from an assumption or
from a third-party tokenizer. The result reflects what a character of content
actually costs on that model. Where a model has no dialect, or confidence is
low, the codec falls back to the classic 4-characters-per-token rate, so it is
never worse than the baseline.

## Why it is a service, not a constant

A model's tokenizer can change with a model update, and no changelog is published
when it does. A fixed table would silently drift out of date. So each dialect is
**refreshed as the models change** and republished. A frozen copy keeps working,
but stops tracking the change; a current copy tracks it. The value of a dialect
is its freshness.

## What we publish, and what we do not

| Published (here, open) | Not published |
|---|---|
| The dialects: per-model `tokens_per_char`, confidence, version. | The measurement data. |
| This methodology and the schema. | The fitting pipeline and its parameters. |
| The fallback rate and the confidence gate. | The scale and source of the telemetry. |

You can read, run, and audit everything in this repository. Reproducing a dialect
would require the private measurement system, which is not part of it.

## Honest limits (what the numbers are, and are not)

- A dialect is a **measured calibration for a model's real usage**. It folds in
  both the tokenizer and the typical content mix.
- It is **not** a statement about a model's internal tokenizer in isolation, and
  it does **not**, on its own, establish whether two models share a tokenizer —
  that requires controlled, identical-content measurement, which is a separate
  step.
- Every published value is fitted from measured counts. No performance or savings
  figure is stated that has not been measured.
