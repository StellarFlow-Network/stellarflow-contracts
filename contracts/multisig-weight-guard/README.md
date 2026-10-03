# Multi-Sig Key Weight Rotation Guard

Implements issue **#977** — *Build Multi-Sig Key Weight Rotation Guard with Mandatory Cooldown*.

In a weighted multi-signature scheme, every signer public key carries an integer
weight and quorum is reached when the sum of the weights of the approving keys
meets a threshold. Rewriting those weights is a high-risk operation: a single
compromised admin key could otherwise rotate the signer set to keys nobody else
agreed to, instantly and invisibly. This contract makes weight rotation **slow**
and **observable**.

## Requirements → implementation

| Issue requirement | Where it lives |
| --- | --- |
| 72-hour cooldown between consecutive signer-weight modifications | `propose_weight_rotation` rejects calls made less than `WEIGHT_ROTATION_COOLDOWN_SECONDS` (72h) after the previous rotation took effect (`GuardError::CooldownNotElapsed`) |
| Current key weights stay valid until the new threshold configuration timestamp is reached | A rotation is **staged**, not applied. `effective_configuration` keeps returning the previous weights until `effective_at = request_time + 72h`; `apply_pending_weights` finalises the switch |
| Emit `SignerWeightsUpdated` event with the full public-key breakdown | `propose_weight_rotation` / `initialize` emit `SignerWeightsUpdated` with the complete `SignerWeight` list, the new threshold, the previous/new `effective_at`, and the actor |

## Lifecycle

```text
initialize ──► active config ───────────────────────────────────────┐
                    ▲                                               │
                    │                           propose (t0)       │
        apply at effective_at ──► pending config (t0 + 72h) ◄───────┘
                    │
                    └──► active config  (cooldown clock restarts)
```

* The cooldown clock is anchored to the moment a rotation **takes effect**, so
  the two guards are independent: `RotationAlreadyPending` stops *overlapping*
  rotations and `CooldownNotElapsed` stops *rapid-fire* ones.
* Between `t0` and `t0 + 72h` the pending configuration exists but is **not in
  force** — quorum continues to be evaluated against the previous weights, so a
  rotation cannot be used to bypass an approval already in flight.
* `apply_pending_weights` is permissionless: it only materialises a decision the
  admin already authorised.

## API

| Function | Purpose |
| --- | --- |
| `initialize(admin, signers, threshold)` | Install the first configuration (immediately effective) |
| `propose_weight_rotation(caller, signers, threshold) -> u64` | Stage a rotation; returns `effective_at` |
| `apply_pending_weights() -> WeightConfiguration` | Finalise once `effective_at` is reached |
| `effective_configuration() -> WeightConfiguration` | Configuration currently enforced |
| `active_configuration()` / `pending_configuration()` | Raw stored state |
| `effective_signer_weight(key)` / `effective_threshold()` | Point reads |
| `verify_quorum(approvals) -> u32` | Sum deduplicated approvals and check the threshold |
| `rotation_cooldown_remaining() -> u64` | Seconds until another rotation may be staged |

## Validation invariants

A submitted configuration is rejected unless it has at least one signer, every
weight is strictly positive, no public key is repeated, `0 < threshold <=
sum(weights)`, and the total weight does not overflow `u32`.

`verify_quorum` ignores unknown keys and counts duplicate approvals only once,
so removed keys cannot keep a stale approval alive.

## Testing

```bash
cargo test -p multisig-weight-guard        # 17 unit tests
cargo clippy -p multisig-weight-guard --all-targets
cargo build -p multisig-weight-guard --target wasm32-unknown-unknown --release
```

Coverage includes: initialisation, double-initialisation rejection, staging vs.
activation of weights, the 72h cooldown (including its exact boundary and
countdown), rejection of premature `apply`, full event payload decoding, quorum
evaluation switching from old to new weights at the activation timestamp, and
every configuration-validation failure mode.
