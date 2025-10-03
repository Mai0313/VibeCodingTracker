FROM rust:1.89.0-alpine AS builder

LABEL maintainer="Wei Lee <mai@mai0313.com>" \
    org.label-schema.name="codex_usage" \
    org.label-schema.vendor="Wei Lee" \
    org.label-schema.schema-version="1.0" \
    com.centurylinklabs.watchtower.stop-signal="SIGINT"

# Install musl-dev for linking dependencies
RUN apk add --no-cache musl-dev pkgconfig openssl-dev openssl-libs-static

WORKDIR /app
COPY . .
RUN cargo build --release --locked

########################################################################################

FROM alpine:latest AS prod
COPY --from=builder /app/target/release/codex_usage /usr/local/bin/codex_usage
ENTRYPOINT ["codex_usage"]
