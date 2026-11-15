# The kite command line as a static binary in an empty image. Build from the repository root:
#   docker build -f packaging/container/Dockerfile -t kite .
# Usage: docs/docker.md.

# The build stage runs on the build machine and cross-compiles, so an arm64 image needs no emulation.
FROM --platform=$BUILDPLATFORM rust:1.100.0-slim-trixie@sha256:e9d95b74296f7126a83ae3d041aeaba1ff4c478f13abc5cc034fcf7c131c3d4a AS build
ARG TARGETARCH
WORKDIR /src
COPY . .
# kite-cli has no C dependencies, so rust-lld links it against Rust's own musl without a C toolchain.
# libloading (through tun2proxy) asks for libdl, which musl keeps in libc.a and ships as an empty
# archive; Rust's copy of musl leaves that archive out, so an empty one stands in.
RUN case "$TARGETARCH" in \
        amd64) target=x86_64-unknown-linux-musl ;; \
        arm64) target=aarch64-unknown-linux-musl ;; \
        *) echo "no Rust target for $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && rustup target add "$target" \
    && mkdir /stub && printf '!<arch>\n' >/stub/libdl.a \
    && cargo build --locked --release -p kite-cli --target "$target" \
        --config "target.$target.linker='rust-lld'" \
        --config "target.$target.rustflags=['-L', '/stub']" \
    && install -D -m 0755 "target/$target/release/kite" /out/usr/local/bin/kite \
    && install -d -m 0700 /out/data /out/run

FROM scratch
COPY --from=build /out/usr /usr
# Owner-only, as kite requires. A new named volume on /data starts as a copy of this directory.
COPY --from=build --chown=65532:65532 /out/data /data
COPY --from=build --chown=65532:65532 /out/run /run/user/65532
LABEL org.opencontainers.image.title="kite" \
      org.opencontainers.image.description="Use a phone's internet connection, including its VPN, from a computer over USB or WiFi" \
      org.opencontainers.image.source="https://github.com/heidari-dev/kite" \
      org.opencontainers.image.licenses="GPL-3.0-or-later"
# Keys and paired phones go to /data/kite; the status socket of a running tunnel to the run directory.
ENV XDG_CONFIG_HOME=/data XDG_RUNTIME_DIR=/run/user/65532
USER 65532:65532
VOLUME /data
# There is no daemon in the container: every command runs in this process.
ENTRYPOINT ["/usr/local/bin/kite", "--foreground"]
CMD ["--help"]
