# Decision-engine experiment

Run the existing deterministic scorer against the authored smoke cases:

```sh
cargo run --manifest-path rust/Cargo.toml -p wayfinder-decision-experiment --locked
```

Exercise the local adapter contract doubles on the identical cases:

```sh
cargo run --manifest-path rust/Cargo.toml -p wayfinder-decision-experiment --locked -- \
  --contract-doubles --allow-external --disclose-prompt
```

This makes **no network requests**. The doubles are explicitly synthetic; they
are not Luna or Decisions API clients. Omitting opt-in or disclosure consent
produces inconclusive results before their adapter is invoked. No production
routing behaviour changes.

JSON receipts contain separate local and provider IDs, engine/model and policy
versions, outcome, elapsed time and correctness. Confidence, distributions and
cost are only recorded if supplied. Unknown cost is null, not zero. Accuracy
includes all cases; answered accuracy excludes inconclusive cases. Both are
reported along with inconclusive rate, mean latency and known-cost coverage.
The deterministic baseline's zero cost means API charges, not machine cost.
Latency is measured locally and UUIDs vary; case text, labels and the policy
are pinned. Compare release builds on the same machine for timing experiments.

The checked-in four-case corpus is a reproducible smoke test, not a benchmark
for product-quality claims. Expected labels never cross the adapter boundary.
A real comparison still needs provider documentation/access and a representative
labelled corpus. See [the design](../designs/WF-DESIGN-0021-decision-engine-experiment.md)
for source verification and adapter/privacy requirements.
