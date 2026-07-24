#!/bin/bash

set -e

VERSION=$(cat .version)
VERSION_WITHOUT_V="${VERSION//v/}"

update_cargo_version() {
  perl -pi -e "s/^version = \"[0-9]+\.[0-9]+\.[0-9]+\"/version = \"$VERSION_WITHOUT_V\"/" "$1"
}

update_flashthing_dep() {
  perl -pi -e "s/(flashthing = \{[^}]*version = \")[^\"]+(\")/\${1}$VERSION_WITHOUT_V\${2}/" "$1"
}

update_cargo_toml() {
  update_cargo_version "$1"
  update_flashthing_dep "$1"
}

update_package_json() {
  perl -pi -e "s/\"version\": \"[0-9]+\.[0-9]+\.[0-9]+\"/\"version\": \"$VERSION_WITHOUT_V\"/" "$1"
}

echo "Setting versions to v$VERSION_WITHOUT_V..."

update_cargo_toml "lib/Cargo.toml"
update_cargo_toml "cli/Cargo.toml"
update_cargo_toml "bindings/Cargo.toml"
update_cargo_toml "wasm/Cargo.toml"
update_package_json "bindings/package.json"

echo "Version updated successfully to v$VERSION_WITHOUT_V"
