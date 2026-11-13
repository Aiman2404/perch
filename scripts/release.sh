#!/usr/bin/env bash
# Build, sign and publish a Perch release to GitHub.
#
#   scripts/release.sh notes.md
#
# Needs: the updater signing key in ~/.tauri/perch-updater.key (+ .password),
# the GitHub CLI logged in, and the version bumped in tauri.conf.json,
# Cargo.toml and package.json. The key never lives in this repository.
set -euo pipefail

notes="${1:?usage: scripts/release.sh <release-notes.md>}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

version="$(node -p "require('./src-tauri/tauri.conf.json').version")"
tag="v$version"
key="$HOME/.tauri/perch-updater.key"
[ -f "$key" ] || { echo "missing $key (see the private notes repo)"; exit 1; }

export TAURI_SIGNING_PRIVATE_KEY="$(cat "$key")"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(cat "$key".password 2>/dev/null || cat "$HOME/.tauri/perch-updater.password")"

echo "Building Perch $version"
npx tauri build

bundle="src-tauri/target/release/bundle/nsis"
setup="$bundle/Perch_${version}_x64-setup.exe"
out="$(mktemp -d)"
cp "$setup" "$out/Perch-Setup-x64.exe"          # stable name for the README download button
cp "$setup" "$out/"

# latest.json tells installed copies what the newest version is and where to get it.
node -e '
const fs = require("fs");
const [version, tag, sigFile, notesFile, outFile] = process.argv.slice(1);
fs.writeFileSync(outFile, JSON.stringify({
  version,
  notes: fs.readFileSync(notesFile, "utf8"),
  pub_date: new Date().toISOString(),
  platforms: { "windows-x86_64": {
    signature: fs.readFileSync(sigFile, "utf8"),
    url: `https://github.com/idlebench/perch/releases/download/${tag}/Perch_${version}_x64-setup.exe`,
  } },
}, null, 2));
' "$version" "$tag" "$setup.sig" "$notes" "$out/latest.json"

gh release create "$tag" "$out"/* --title "Perch $version" --notes-file "$notes" --latest
echo "Published $tag"
