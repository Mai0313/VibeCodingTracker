FROM rust:1.89.0-bookworm AS builder

LABEL maintainer="Wei Lee <mai@mai0313.com>" \
    org.label-schema.name="vibe_coding_tracker" \
    org.label-schema.vendor="Wei Lee" \
    org.label-schema.schema-version="1.0" \
    com.centurylinklabs.watchtower.stop-signal="SIGINT"

WORKDIR /app
COPY . .
RUN cargo build --release --locked

########################################################################################

FROM ubuntu:24.04 AS prod
COPY --from=builder /app/target/release/vibe_coding_tracker /usr/local/bin/vibe_coding_tracker
ENTRYPOINT ["vibe_coding_tracker"]
