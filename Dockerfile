# syntax=docker/dockerfile:1.7

# CodoSEO image: one static binary on distroless. Built natively (musl) on each architecture, so
# the same file works on amd64 and arm64 hosts without QEMU or cross toolchains.
#
#   docker build -t codoseo:local .
#   docker run --rm codoseo:local crawl https://example.com
#
# The roles (`all`, `web`, `worker`, `migrate`) and the CLI commands are arguments to the entrypoint.

# Keep in step with rust-toolchain.toml.
FROM rust:1.99.0-alpine AS build

# musl-dev + a C toolchain for mimalloc and the TLS crates' C code (ring, aws-lc-sys); perl and
# cmake are only needed if aws-lc-sys decides to build with them.
RUN apk add --no-cache musl-dev build-base perl cmake

WORKDIR /src
# rust-toolchain.toml is deliberately not copied: the base image already is the pinned toolchain.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# The dist profile (thin LTO, one codegen unit, stripped) is defined in Cargo.toml. The cache
# mounts keep the registry and build artefacts between builds; the binary is copied out of the
# target cache in the same step.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --locked --profile dist -p codoseo \
    && install -D target/dist/codoseo /out/codoseo

FROM gcr.io/distroless/static-debian12:nonroot

LABEL org.opencontainers.image.title="CodoSEO" \
      org.opencontainers.image.description="A fast, polite SEO crawler and site auditor, with a web app that tells you the moment your SEO breaks." \
      org.opencontainers.image.source="https://github.com/SafrowLabs/codoSEO" \
      org.opencontainers.image.url="https://codoseo.com" \
      org.opencontainers.image.licenses="AGPL-3.0-only" \
      org.opencontainers.image.vendor="SafrowLabs"

COPY --from=build /out/codoseo /codoseo

EXPOSE 8080
ENTRYPOINT ["/codoseo"]
CMD ["all"]

# Only meaningful for the web-serving roles (`all`, `web`); compose files for `worker` and
# `migrate` turn it off.
HEALTHCHECK --interval=15s --timeout=5s --start-period=20s --retries=3 \
    CMD ["/codoseo", "healthcheck"]
