# truenas-nvidia-driver-manager — the empty-base image of the App container; the
# build container is the fingerprint-pinned official Debian image plus the fixed
# package closure on the host.
#
# Multi-stage, base images pinned as tag@digest; Cargo.lock and web/package-lock.json
# are committed, so the image builds with --locked and npm ci. Renovate updates the
# digests and lockfiles.

# Web assets.
FROM node:24-bookworm-slim@sha256:a5f4366e834695d5f363b920c104953a535d3a9864d1bdf74566a0ebe01daee5 AS web
WORKDIR /build
COPY web/package.json web/package-lock.json ./web/
RUN npm --prefix web ci
COPY web ./web
RUN npm --prefix web exec -- tsc -p web
# index.html and styles.css are not tsc inputs; copy them into dist.
RUN cp web/index.html web/src/styles.css web/dist/ \
 && mkdir -p web/dist/brand \
 && cp web/brand/*.svg web/dist/brand/

# Rust static executable.
FROM rust:1-bookworm@sha256:e12566d5abc17a0da741628782cdb504b68b8961f5bfcf677936bf3e8b32de29 AS rust
RUN rustup target add x86_64-unknown-linux-musl
# Host execution requires a static executable; the dependency tree needs no C compiler.
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static"
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# The program embeds the build script and the kit installer (`include_str!`), so
# both must be present at compile time.
COPY docker ./docker
RUN cargo build --release --locked --target x86_64-unknown-linux-musl
# Generate notices from the exact locked sources used by both build stages.
COPY tools/third-party-licenses.py ./tools/
COPY web/package.json web/package-lock.json ./web/
COPY --from=web /build/web/node_modules ./web/node_modules
RUN command -v python3 >/dev/null 2>&1 || \
    { echo "Runtime license generation requires python3 in the Rust build image." >&2; exit 1; }
RUN python3 tools/third-party-licenses.py
# The runner executes this artifact directly on the host, so it must carry no ELF
# interpreter. The intermediate file keeps the check fail-closed.
RUN BIN=target/x86_64-unknown-linux-musl/release/truenas-nvidia-driver-manager && \
    readelf -l "$BIN" > /tmp/elf-headers && \
    ! grep -qi 'Requesting program interpreter' /tmp/elf-headers

# The runtime carries no distribution userland.
FROM scratch
LABEL org.opencontainers.image.source="https://github.com/sky-zhang01/truenas-nvidia-driver-manager" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.title="TrueNAS NVIDIA Driver Manager" \
      org.opencontainers.image.description="Manage NVIDIA driver updates on a TrueNAS SCALE host through a web UI"
COPY --from=rust /build/target/x86_64-unknown-linux-musl/release/truenas-nvidia-driver-manager \
                 /usr/local/bin/truenas-nvidia-driver-manager
COPY --from=web /build/web/dist/ /usr/local/share/truenas-nvidia-driver-manager/web/
COPY LICENSE THIRD_PARTY_NOTICES.md /usr/local/share/truenas-nvidia-driver-manager/
COPY --from=rust /build/THIRD_PARTY_LICENSES.txt /usr/local/share/truenas-nvidia-driver-manager/
# The production path needs no distribution file or directory inside the image:
# host commands (unsquashfs and dpkg-deb included) run on the host through the
# program's own namespace mode and temporary files
# only land in the data directory, so no empty directory such as /tmp is added.
# The /proc and /dev mounts plus the /etc/hostname, /etc/hosts and
# /etc/resolv.conf bind mounts come from the runtime and need no image file.
# The startup check reads the /etc/hostname row of /proc/self/mountinfo, and the
# CI container job asserts it.
# The build script and the build-kit installer are not in the image: the program
# embeds both and writes them into the task's `in` directory before a build.
# No USER: in the privileged container the web process runs host commands as root
# through the program's own namespace mode.
ENTRYPOINT ["/usr/local/bin/truenas-nvidia-driver-manager"]
