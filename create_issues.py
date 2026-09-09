#!/usr/bin/env python3
"""Thin wrapper around scripts/maintainer/create_issues.py.

Delegates to the canonical maintainer script which handles rate-limit
retries and duplicate-issue guards.
"""

import os
import subprocess
import sys

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
MAINTAINER_SCRIPT = os.path.join(SCRIPT_DIR, "scripts", "maintainer", "create_issues.py")

sys.exit(subprocess.call([sys.executable, MAINTAINER_SCRIPT] + sys.argv[1:]))
