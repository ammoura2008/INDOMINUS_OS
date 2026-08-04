import os
import sys
from pathlib import Path

root = Path(__file__).resolve().parent.parent

required = [
    root / '.vscode' / 'tasks.json',
    root / '.vscode' / 'launch.json',
    root / '.github' / 'workflows' / 'ci.yml',
    root / 'docs' / 'ROADMAP_BOARD.md',
]

missing = [str(p.relative_to(root)) for p in required if not p.exists()]
if missing:
    print('Missing required workspace files:')
    for item in missing:
        print(f' - {item}')
    sys.exit(1)

print('Workspace scaffolding OK')
