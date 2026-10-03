"""Wrap failing tests' bodies in env.as_contract (storage needs contract ctx).

Insertion point: immediately after the statement that binds `env`
(either `let env = Env::default();` or a fixture `let (env, ...) = ...;`).
Closure closes right before the test fn's closing brace.
"""
import re, subprocess, sys

def find_fn_span(src, name):
    pat = re.compile(r'fn\s+' + re.escape(name) + r'\s*\(')
    m = pat.search(src)
    if not m: return None
    open_idx = m.end() - 1
    depth = 0
    i = open_idx
    while i < len(src):
        if src[i] == '{': depth += 1
        elif src[i] == '}':
            depth -= 1
            if depth == 0: return (open_idx, i)
        i += 1
    return None

def find_env_bind(body):
    """Return index just after the line that first binds env."""
    pats = [
        re.compile(r'^[ \t]*let\s+env\s*=\s*Env::default\(\);.*$', re.M),
        re.compile(r'^[ \t]*let\s+env\s*=\s*crate::Env::default\(\);.*$', re.M),
        re.compile(r'^[ \t]*let\s*\(env,.*?=\s*setup\([^;]*\);', re.M | re.S),
        re.compile(r'^[ \t]*let\s*\(env,.*?\)\s*=\s*[a-zA-Z_:]+\([^;]*\);', re.M | re.S),
    ]
    for p in pats:
        m = p.search(body)
        if m: return m.end()
    return None

def wrap_test(src, fn_name):
    span = find_fn_span(src, fn_name)
    if not span: return src, 'missing'
    open_idx, close_idx = span
    body = src[open_idx+1:close_idx]
    if 'as_contract' in body:
        return src, 'already'
    bind = find_env_bind(body)
    if bind is None:
        return src, 'no-env'
    indent_m = re.match(r'[ \t]*', body[bind:bind+1] and body[bind:] or '')
    # find indentation of the next line
    nl = body.find('\n', bind)
    if nl == -1: return src, 'no-env'
    m2 = re.match(r'([ \t]*)', body[nl+1:])
    ind = m2.group(1) if m2 else '        '
    ins = (f"\n{ind}let cid = env.register_contract(None, crate::TimeLockedUpgradeContract);"
           f"\n{ind}env.as_contract(&cid, || {{")
    new_body = body[:bind] + ins + body[bind:] + f"\n{ind}}});"
    return src[:open_idx+1] + new_body + src[close_idx:], 'ok'

MODMAP = {
 'action_guard':'src/action_guard.rs',
 'recovery':'src/recovery.rs',
 'security::reentrancy':'src/security/reentrancy.rs',
 'security::pausable':'src/security/pausable.rs',
 'security::auth_guard':'src/security/auth_guard.rs',
 'auth::dispatcher':'src/auth/dispatcher.rs',
 'router::multihop':'src/router/multihop.rs',
 'router::dynamic':'src/router/dynamic.rs',
 'amm::ticks':'src/amm/ticks.rs',
 'amm::invariant':'src/amm/invariant.rs',
 'amm::circuit_breaker':'src/amm/circuit_breaker.rs',
 'amm::adaptive_fee':'src/amm/adaptive_fee.rs',
 'amm::deviation_guard':'src/amm/deviation_guard.rs',
 'amm::slippage':'src/amm/slippage.rs',
 'escrow::timelock':'src/escrow/timelock.rs',
 'escrow::merkle':'src/escrow/merkle.rs',
 'zk::merkle':'src/zk/merkle.rs',
 'zk::batch_insert':'src/zk/batch_insert.rs',
 'zk::nullifier':'src/zk/nullifier.rs',
 'admin::cleanup':'src/admin/cleanup.rs',
 'admin::prune':'src/admin/prune.rs',
 'admin::action_queue':'src/admin/action_queue.rs',
 'fees':'src/fees.rs',
 'config':'src/config.rs',
 'flash_loan_guard':'src/flash_loan_guard.rs',
 'roles':'src/roles.rs',
 'veto':'src/veto.rs',
 'events::swaps':'src/events/swaps.rs',
 'events::liquidity':'src/events/liquidity.rs',
 'events::events':'src/events/events.rs',
 'test':'src/test.rs',
 'vaults':'src/vaults/mod.rs',
 'consensus':'src/consensus.rs',
 'oracle_attestation':'src/oracle_attestation.rs',
 'multisig_expiry':'src/multisig_expiry.rs',
}

def run():
    out = subprocess.run(
        ['cargo','test','--lib','--','--test-threads=1'],
        capture_output=True, text=True, encoding='utf-8', errors='replace'
    )
    txt = out.stdout + out.stderr
    failed = []
    for l in txt.split('\n'):
        m = re.match(r'test (\S+) \.\.\. FAILED$', l)
        if m: failed.append(m.group(1))
    if not failed:
        print("NO FAILURES")
        return
    print(f"{len(failed)} failing tests")
    byfile = {}
    for t in failed:
        parts = t.split('::')
        name = parts[-1]
        # Path may be prefixed by the crate name; try suffixes.
        f = None
        for start in range(len(parts)):
            suffix = parts[start:-1]
            while suffix:
                key = '::'.join(suffix)
                if key in MODMAP:
                    f = MODMAP[key]; break
                suffix.pop()
            if f: break
        if f:
            byfile.setdefault(f, set()).add(name)
        else:
            print("unmapped:", t)
    for f, names in byfile.items():
        src = open(f, encoding='utf-8').read()
        stats = {}
        for n in names:
            src, status = wrap_test(src, n)
            stats[status] = stats.get(status, 0) + 1
        open(f, 'w', encoding='utf-8', newline='').write(src)
        print(f, dict(stats))

if __name__ == '__main__':
    run()
