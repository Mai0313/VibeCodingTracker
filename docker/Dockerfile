FROM rust:1.95.0-bookworm AS builder

LABEL maintainer="Wei Lee <mai@mai0313.com>" \
    org.label-schema.name="vibe_coding_tracker" \
    org.label-schema.vendor="Wei Lee" \
    org.label-schema.schema-version="1.0" \
    com.centurylinklabs.watchtower.stop-signal="SIGINT"

WORKDIR /app
COPY . .

# Cache mounts:
#   /usr/local/cargo/registry, /usr/local/cargo/git -> avoid re-downloading crates
#   /app/target                                     -> preserve incremental build artifacts
# /app/target lives in the cache mount, so we cp the binary out to a real layer path
# before the cache detaches; the prod stage COPYs from that path.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked \
    && cp target/release/vibe_coding_tracker /usr/local/bin/vibe_coding_tracker

########################################################################################

FROM ubuntu:26.04 AS prod
COPY --from=builder /usr/local/bin/vibe_coding_tracker /usr/local/bin/vibe_coding_tracker
ENTRYPOINT ["vibe_coding_tracker"]
