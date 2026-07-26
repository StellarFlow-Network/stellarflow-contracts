# Init args files

`scripts/deploy.sh` looks here for `<contract>.args` when `--init-args` /
`--init-args-file` isn't passed on the command line.

Each file holds the CLI arguments forwarded to the contract's `initialize`
call, one flag per line. Lines starting with `#` are ignored. Example for
`price-oracle` (`price-oracle.args`):

```
--admin GABCDEF...
--base_currency_pairs '["NGN","KES","GHS"]'
```

Values are network-specific (admin addresses, token IDs, etc.), so keep
real deployment values out of version control if they're sensitive - pass
them via `--init-args` at deploy time instead, or use an untracked local
copy of this file.
