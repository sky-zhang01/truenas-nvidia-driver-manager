#!/usr/bin/env bash
# build-kit.sh — the pinned Debian package closure of the build environment.
#
# The program's build environment is the official `debian:bookworm-slim` image (pinned by its
# index digest) plus a fixed Debian package closure taken from one frozen snapshot.debian.org
# timestamp. This tool is the single source of that closure:
#
# tools/build-kit.sh gen [FILE]   Resolve the closure and write the manifest (default: stdout).
# tools/build-kit.sh check        Regenerate and byte-compare src/build_kit/packages.tsv.
# tools/build-kit.sh fetch DIR    Download the packages and verify byte counts and SHA256.
#
# `gen` and `check` need network access (snapshot.debian.org, plus Docker Hub when the image
# is not present yet) and a docker daemon; `fetch` needs curl only. When TrueNAS moves to
# Debian 13, update the image, the timestamp and the top-level package list below, re-run
# `gen` and commit the new manifest.
set -Eeuo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="${REPO_ROOT}/src/build_kit/packages.tsv"

# The pinned build environment: the same values the program carries (a Rust test
# cross-checks them).
IMAGE_REF="debian@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587"
IMAGE_ID="sha256:d4c45f66efc59f89b0597d343a9bd72808493c0eacecde5285961743252cee23"
IMAGE_INDEX_ID="sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587"
SNAPSHOT="20261005T000000Z"
TOP_PACKAGES="gcc-12 binutils make libc6-dev kmod squashfs-tools libelf-dev patch xz-utils"

# The local image ID: a classic image store reports the per-platform image ID
# (the value measured on the target host), a containerd-backed store reports
# the index digest we pulled by. Both name the same pinned amd64 content;
# anything else is refused.
image_id_ok() {
    case "$1" in
        "$IMAGE_ID" | "$IMAGE_INDEX_ID") return 0 ;;
        *) return 1 ;;
    esac
}

# Allows a docker invocation with arguments (for local runs against another context).
read -r -a DOCKER_CMD <<<"${NVM_DOCKER:-docker}"
DOCKER_PLATFORM="linux/amd64"

log() { printf 'build-kit: %s\n' "$*" >&2; }
die() { printf 'build-kit: %s\n' "$*" >&2; exit 1; }

# Use the local
# image when it is there; pull only when it is missing; verify the local image ID either way.
ensure_image() {
    local actual=""
    if actual="$("${DOCKER_CMD[@]}" image inspect -f '{{.Id}}' "${IMAGE_REF}" 2>/dev/null)"; then
        image_id_ok "${actual}" || die "local image ${IMAGE_REF} has ID ${actual}, expected ${IMAGE_ID}"
        log "local image ${IMAGE_REF} (${actual})"
        return 0
    fi
    log "pulling ${IMAGE_REF}"
    "${DOCKER_CMD[@]}" pull --platform "${DOCKER_PLATFORM}" "${IMAGE_REF}" >&2
    actual="$("${DOCKER_CMD[@]}" image inspect -f '{{.Id}}' "${IMAGE_REF}")"
    image_id_ok "${actual}" || die "pulled image ${IMAGE_REF} has ID ${actual}, expected ${IMAGE_ID}"
    log "pulled image ${IMAGE_REF} (${actual})"
}

# Resolve the closure inside the pinned image at the snapshot timestamp and print
# the manifest data rows (the caller writes the header).
closure_rows() {
    "${DOCKER_CMD[@]}" run --rm --pull never --platform "${DOCKER_PLATFORM}" \
        --network bridge \
        --env "NVM_SNAPSHOT=${SNAPSHOT}" \
        --env "NVM_TOP_PACKAGES=${TOP_PACKAGES}" \
        "${IMAGE_REF}" /bin/bash -c "$(cat <<'INNER'
set -Eeuo pipefail
export DEBIAN_FRONTEND=noninteractive
cat > /etc/apt/apt.conf.d/80-download-resilience <<'EOF'
Acquire::Retries "5";
Acquire::http::Timeout "30";
Acquire::https::Timeout "30";
EOF
# Keep only the signed indexes of the snapshot timestamp: apt still verifies the
# signatures, only the (necessarily stale) Valid-Until check is turned off.
rm -f /etc/apt/sources.list.d/debian.sources
cat > /etc/apt/sources.list.d/snapshot.sources <<EOF
Types: deb
URIs: http://snapshot.debian.org/archive/debian/${NVM_SNAPSHOT}
Suites: bookworm bookworm-updates
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg
Check-Valid-Until: no

Types: deb
URIs: http://snapshot.debian.org/archive/debian-security/${NVM_SNAPSHOT}
Suites: bookworm-security
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg
Check-Valid-Until: no
EOF
apt-get -qq update >&2
# Decode `%xx`: apt escapes the epoch, `+` and `~` in an address, while the pool
# path of the index is plain text.
decode_uri() {
    printf '%b' "$(printf '%s' "$1" | sed 's/%\([0-9a-fA-F][0-9a-fA-F]\)/\\x\1/g')"
}
# First take the download addresses apt itself would use: every one comes from
# the signed Packages index of this timestamp — never assembled by hand.
apt-get install -y --print-uris --no-install-recommends --download-only ${NVM_TOP_PACKAGES} \
    > /tmp/uris 2>/dev/null || true
grep "^'" /tmp/uris | awk -F"'" '{print $2}' | LC_ALL=C sort -u > /tmp/urls
[ -s /tmp/urls ] || { echo "build-kit: apt printed no download address" >&2; exit 1; }
apt-get install -y -qq --no-install-recommends --download-only ${NVM_TOP_PACKAGES} >&2
cd /var/cache/apt/archives
: > /tmp/rows
for deb in *.deb; do
    pkg="$(dpkg-deb -f "${deb}" Package)"
    ver="$(dpkg-deb -f "${deb}" Version)"
    # Filename/Size/SHA256 come from the signed Packages index of this timestamp.
    fields="$(apt-cache show "${pkg}=${ver}")" \
        || { echo "build-kit: no index entry for ${pkg}=${ver}" >&2; exit 1; }
    pool="$(printf '%s\n' "${fields}" | sed -n 's/^Filename: //p' | head -1)"
    index_size="$(printf '%s\n' "${fields}" | sed -n 's/^Size: //p' | head -1)"
    index_sha="$(printf '%s\n' "${fields}" | sed -n 's/^SHA256: //p' | head -1)"
    [ -n "${pool}" ] && [ -n "${index_size}" ] && [ -n "${index_sha}" ] \
        || { echo "build-kit: ${pkg}=${ver} has no complete index entry" >&2; exit 1; }
    # The downloaded bytes are the ones the index claims: both the byte count
    # and the SHA256 must match before the row is written.
    file_size="$(stat -c %s "${deb}")"
    file_sha="$(sha256sum "${deb}" | cut -d' ' -f1)"
    [ "${file_size}" = "${index_size}" ] \
        || { echo "build-kit: ${deb} is ${file_size} bytes, index says ${index_size}" >&2; exit 1; }
    [ "${file_sha}" = "${index_sha}" ] \
        || { echo "build-kit: ${deb} has SHA256 ${file_sha}, index says ${index_sha}" >&2; exit 1; }
    # The address must be exactly one apt listed: apt's address is
    # percent-decoded and compared literally with "archive base + the pool path
    # from the index"; exactly one hit is allowed. The manifest records https
    # (downloads use https, like the deployment guide).
    url=""
    while IFS= read -r candidate; do
        case "$(decode_uri "${candidate}")" in
            "http://snapshot.debian.org/archive/debian/${NVM_SNAPSHOT}/${pool}" | \
                "http://snapshot.debian.org/archive/debian-security/${NVM_SNAPSHOT}/${pool}")
                [ -z "${url}" ] \
                    || { echo "build-kit: ${pkg}=${ver} matches more than one archive" >&2; exit 1; }
                url="https://${candidate#http://}"
                ;;
        esac
    done < /tmp/urls
    [ -n "${url}" ] \
        || { echo "build-kit: apt listed no address for ${pkg}=${ver} (${pool})" >&2; exit 1; }
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
        "${index_sha}" "${index_size}" "${url##*/}" "${pkg}" "${ver}" "${url}" >> /tmp/rows
done
[ "$(wc -l < /tmp/rows | tr -d ' ')" = "$(wc -l < /tmp/urls | tr -d ' ')" ] \
    || { echo "build-kit: $(wc -l < /tmp/rows) package(s) for $(wc -l < /tmp/urls) address(es)" >&2; exit 1; }
dups="$(cut -f3 /tmp/rows | LC_ALL=C sort | uniq -d)"
[ -z "${dups}" ] || { echo "build-kit: duplicate file name(s): ${dups}" >&2; exit 1; }
LC_ALL=C sort -t "$(printf '\t')" -k3,3 /tmp/rows
INNER
)"
}

manifest_header() { # manifest_header <package-count>
    cat <<EOF
# truenas-nvidia-driver-manager build kit — generated by tools/build-kit.sh gen; do not edit.
# image: ${IMAGE_REF}
# image_id: ${IMAGE_ID}
# snapshot: ${SNAPSHOT}
# top_packages: ${TOP_PACKAGES}
# columns: sha256, bytes, filename, package, version, url (tab-separated)
# packages: $1
EOF
}

# ── gen / check ───────────────────────────────────────────────────────────────
cmd_gen() {
    local out="${1:-}"
    ensure_image
    local rows count
    rows="$(closure_rows)" || die "resolving the closure failed"
    count="$(printf '%s\n' "${rows}" | grep -c . || true)"
    [ "${count}" -gt 0 ] || die "the closure is empty"
    local text
    text="$(manifest_header "${count}")
${rows}"
    if [ -n "${out}" ]; then
        printf '%s\n' "${text}" > "${out}"
        log "wrote ${out} (${count} packages)"
    else
        printf '%s\n' "${text}"
    fi
}

# The scratch directory of `check` is a global so the EXIT cleanup can still see
# it (a function-local would already be gone at that point).
CHECK_WORK=""
check_cleanup() { # check_cleanup — EXIT trap
    [ -n "${CHECK_WORK}" ] && rm -rf "${CHECK_WORK}"
    return 0
}

cmd_check() {
    [ -f "${MANIFEST}" ] || die "manifest ${MANIFEST} is missing"
    CHECK_WORK="$(mktemp -d)"
    trap check_cleanup EXIT
    cmd_gen "${CHECK_WORK}/packages.tsv"
    if diff -u "${MANIFEST}" "${CHECK_WORK}/packages.tsv"; then
        log "check: ${MANIFEST} matches the closure resolved in ${IMAGE_REF}"
    else
        die "check: ${MANIFEST} differs from the closure resolved in ${IMAGE_REF} (see the diff above)"
    fi
}

# ── fetch ─────────────────────────────────────────────────────────────────────
# One-time preparation for an offline host: download every manifest package into
# DIR and verify its byte count and SHA256.
cmd_fetch() {
    local dir="${1:-}"
    [ -n "${dir}" ] || die "fetch needs an output directory"
    [ -f "${MANIFEST}" ] || die "manifest ${MANIFEST} is missing"
    mkdir -p "${dir}"
    local count=0
    local sha size filename package version url target
    while IFS="$(printf '\t')" read -r sha size filename package version url; do
        case "${sha}" in "" | "#"*) continue ;; esac
        case "${url}" in
            https://snapshot.debian.org/*) ;;
            *) die "unexpected download address ${url}" ;;
        esac
        target="${dir}/${filename}"
        if [ -f "${target}" ] \
            && [ "$(stat -c %s "${target}" 2>/dev/null || stat -f %z "${target}")" = "${size}" ] \
            && [ "$(sha256sum "${target}" | cut -d' ' -f1)" = "${sha}" ]; then
            log "present ${filename}"
            count=$((count + 1))
            continue
        fi
        log "fetching ${filename} (${package} ${version})"
        curl -fsS --proto '=https' --location \
            --retry 5 --retry-delay 3 --retry-connrefused \
            --connect-timeout 15 --max-time 300 \
            -o "${target}.part" "${url}"
        local got_sha got_size
        got_sha="$(sha256sum "${target}.part" | cut -d' ' -f1)"
        got_size="$(stat -c %s "${target}.part" 2>/dev/null || stat -f %z "${target}.part")"
        [ "${got_size}" = "${size}" ] || die "${filename}: got ${got_size} bytes, expected ${size}"
        [ "${got_sha}" = "${sha}" ] || die "${filename}: got SHA256 ${got_sha}, expected ${sha}"
        mv "${target}.part" "${target}"
        count=$((count + 1))
    done < "${MANIFEST}"
    [ "${count}" -gt 0 ] || die "the manifest has no package row"
    log "fetch: ${count} package(s) verified in ${dir}"
}

case "${1:-}" in
    gen) shift; cmd_gen "${1:-}" ;;
    check) shift; [ "$#" -eq 0 ] || die "check takes no argument"; cmd_check ;;
    fetch) shift; cmd_fetch "${1:-}" ;;
    *)
        printf 'usage: %s gen [FILE] | check | fetch DIR\n' "$0" >&2
        exit 2
        ;;
esac
