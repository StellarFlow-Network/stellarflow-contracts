import os
import glob

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
        else:
            out.append(lines[i])
        i += 1
        
    with open(filepath, 'w') as f:
        f.write('\n'.join(out))

resolve_file('src/auth.rs', 'theirs')
resolve_file('src/governance.rs', 'theirs')

# Resolving test snapshots with ours
for file in glob.glob('test_snapshots/**/*.json', recursive=True):
    try:
        resolve_file(file, 'ours')
    except Exception:
        pass
