#!/bin/sh
# Installs pion's release binaries.
#
#   curl -fsSL https://raw.githubusercontent.com/quantum-box/pion/<tag>/scripts/install.sh | sh
#
# Every download is checked against the release's SHA256SUMS before anything
# is installed.
#
#   PION_VERSION         release to install, such as v0.1.0 (default: latest)
#   PION_INSTALL_DIR     where the binaries go (default: $CARGO_HOME/bin or ~/.cargo/bin)
#   PION_RUSTC_NIGHTLY   also install the rustc wrapper built for this nightly,
#                        such as nightly-2026-06-04 (default: not installed)
set -eu

repository="quantum-box/pion"
version="${PION_VERSION:-latest}"
install_dir="${PION_INSTALL_DIR:-${CARGO_HOME:-$HOME/.cargo}/bin}"
nightly="${PION_RUSTC_NIGHTLY:-}"

fail() {
	echo "pion install: $1" >&2
	exit 1
}

case "$version" in
latest) base="https://github.com/$repository/releases/latest/download" ;;
v[0-9]*) base="https://github.com/$repository/releases/download/$version" ;;
*) fail "PION_VERSION must be 'latest' or a tag like v0.1.0, not '$version'" ;;
esac
case "$nightly" in
"" | nightly-[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) ;;
*) fail "PION_RUSTC_NIGHTLY must look like nightly-2026-06-04, not '$nightly'" ;;
esac

os="$(uname -s)"
arch="$(uname -m)"
case "$arch" in
x86_64 | amd64) arch="x86_64" ;;
arm64 | aarch64) arch="aarch64" ;;
*) fail "no release for the architecture '$arch'" ;;
esac
case "$os" in
Linux)
	cli_target="$arch-unknown-linux-musl"
	driver_target="$arch-unknown-linux-gnu"
	;;
Darwin)
	[ "$arch" = "aarch64" ] || fail "no release for macOS on '$arch'"
	cli_target="aarch64-apple-darwin"
	driver_target="aarch64-apple-darwin"
	;;
*) fail "no release for the system '$os'" ;;
esac

if command -v sha256sum >/dev/null 2>&1; then
	checksum() { sha256sum "$1" | cut -d ' ' -f 1; }
elif command -v shasum >/dev/null 2>&1; then
	checksum() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
else
	fail "neither sha256sum nor shasum is available to verify the download"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fetch() {
	curl --fail --silent --show-error --location --retry 3 --output "$work/$1" "$base/$1" ||
		fail "could not download $1 from $base"
}

# Downloads an archive, verifies it and unpacks its one binary.
install_archive() {
	archive="$1"
	binary="$2"
	fetch "$archive"
	expected="$(awk -v name="$archive" '$2 == name || $2 == "*" name { print $1 }' "$work/SHA256SUMS")"
	[ -n "$expected" ] || fail "$archive is not listed in SHA256SUMS"
	actual="$(checksum "$work/$archive")"
	[ "$expected" = "$actual" ] || fail "checksum mismatch for $archive"
	tar -xzf "$work/$archive" -C "$work" "$binary"
	mkdir -p "$install_dir"
	mv "$work/$binary" "$install_dir/$binary"
	chmod +x "$install_dir/$binary"
	echo "installed $install_dir/$binary"
}

fetch SHA256SUMS
install_archive "cargo-pion-$cli_target.tar.gz" cargo-pion
if [ -n "$nightly" ]; then
	install_archive "pion-rustc-$nightly-$driver_target.tar.gz" pion-rustc
fi
