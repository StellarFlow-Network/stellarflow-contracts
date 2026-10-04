# `ethnum` compatibility shim

## Why this exists

`soroban-env-common 20.0.0` (pulled in by `soroban-sdk =20.0.0`, which this
workspace pins) declares:

```toml
[dependencies.ethnum]
version = "=1.5.0"
```

ethnum 1.5.0 constructs the standard-library error types it returns with
`mem::transmute`, including:

```rust
pub const fn tfie() -> TryFromIntError {
    unsafe { mem::transmute(()) }
}
```

That transmute is only valid while `TryFromIntError` is zero-sized. Current
stable Rust made it one byte, so the code is now a hard error:

```
error[E0512]: cannot transmute between types of different sizes, or dependently-sized types
  --> ethnum-1.5.0/src/error.rs:16:14
   |
16 |     unsafe { mem::transmute(()) }
   |              ^^^^^^^^^^^^^^
```

Because the dependency requirement is an **exact** pin, upgrading the locked
`ethnum` version cannot fix this: Cargo always re-resolves to 1.5.0 and the
whole workspace fails to build.

## What this shim does

Upstream fixed the transmute in ethnum 1.5.3
([nlordell/ethnum-rs#58](https://github.com/nlordell/ethnum-rs/pull/58)).
This directory is a tiny crate that:

1. advertises `name = "ethnum"`, `version = "1.5.0"` so it satisfies the exact
   pin, and
2. re-exports the fixed upstream `1.5.3` release with `pub use ethnum_upstream::*;`.

The workspace root selects it with:

```toml
[patch.crates-io]
ethnum = { path = "third-party/ethnum-compat" }
```

The `ethnum` public API seen by `soroban-env-common` and friends is unchanged,
because every public item is re-exported directly from upstream. Nothing in this
repository is patched or forked; only the version pin is bridged.

## Removing it

Delete this directory and the `[patch.crates-io]` entry once the pinned
`soroban-sdk` / `soroban-env-common` pair allows an `ethnum` version `>= 1.5.3`
(in which case Cargo reports the unused patch and ignores it).
