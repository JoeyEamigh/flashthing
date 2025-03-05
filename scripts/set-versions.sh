#!/bin/bash

set -e

VERSION=$(cat .version)
VERSION_WITHOUT_V="${VERSION//v/}"

update_cargo_toml() {
  local file=$1
  sed -i "s/^version = \"[0-9]*\.[0-9]*\.[0-9]*\"/version = \"$VERSION_WITHOUT_V\"/" "$file"
}

update_package_json() {
  local file=$1
  sed -i "s/\"version\": \"[0-9]*\.[0-9]*\.[0-9]*\"/\"version\": \"$VERSION_WITHOUT_V\"/" "$file"
}

echo "Setting versions to v$VERSION_WITHOUT_V..."

update_cargo_toml "lib/Cargo.toml"
update_cargo_toml "cli/Cargo.toml"
update_cargo_toml "bindings/Cargo.toml"
update_package_json "bindings/package.json"

echo "Version updated successfully to v$VERSION_WITHOUT_V"
