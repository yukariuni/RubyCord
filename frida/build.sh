#!/bin/bash
# Bundle the Frida bootstrap (with frida-java-bridge) into web/plugins/frida-bootstrap.js.
set -euo pipefail
cd "$(dirname "$0")"
npm install --no-audit --no-fund --silent
npm run --silent build
