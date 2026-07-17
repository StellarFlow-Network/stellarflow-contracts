import os

def resolve_file(filepath, strategy):
    with open(filepath, 'r') as f:
        content = f.read()
    
    out = []
    lines = content.split('\n')
    i = 0
    while i < len(lines):
        if lines[i].startswith('<<<<<<<'):
            head = []
            upstream = []
            i += 1
            while not lines[i].startswith('======='):
                head.append(lines[i])
                i += 1
            i += 1
            while not lines[i].startswith('>>>>>>>'):
                upstream.append(lines[i])
                i += 1
            
            if strategy == 'ours':
                out.extend(head)
            elif strategy == 'theirs':
                out.extend(upstream)
            elif strategy == 'both':
                out.extend(head)
                out.extend(upstream)
            elif strategy == 'smart_lib':
                # Custom logic for lib.rs
                if "symbol_to_asset_id" in "\n".join(upstream):
                    out.extend(upstream) # Use upstream's symbol_to_asset_id
                elif "EpochClosed" in "\n".join(head):
                    out.extend(head)
                    out.extend(upstream)
                else:
                    out.extend(head)
                    out.extend(upstream)
            elif strategy == 'smart_test':
                # Custom logic for test.rs
                if "std::println" in "\n".join(head):
                    out.extend(upstream) # Drop our debug prints
                else:
                    out.extend(head) # keep our sequence fixes
        else:
            out.append(lines[i])
        i += 1
        
    with open(filepath, 'w') as f:
        f.write('\n'.join(out))

resolve_file('src/auth.rs', 'theirs')
resolve_file('src/governance.rs', 'theirs')
resolve_file('src/consensus.rs', 'both')
resolve_file('src/lib.rs', 'smart_lib')
resolve_file('src/test.rs', 'smart_test')

# For snapshots, we'll just accept ours because the test runner will regenerate them anyway
import glob
for file in glob.glob('test_snapshots/**/*.json', recursive=True):
    try:
        resolve_file(file, 'ours')
    except Exception:
        pass
