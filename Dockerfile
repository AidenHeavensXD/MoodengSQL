# Build stage
FROM rust:1.85-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY moodeng-core ./moodeng-core
COPY moodeng-server ./moodeng-server
COPY moodeng-cli ./moodeng-cli
RUN cargo build --release -p moodeng-server -p moodeng-cli

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/moodengsql /usr/local/bin/moodengsql
COPY --from=builder /app/target/release/moodeng /usr/local/bin/moodeng
COPY moodeng.toml.example /etc/moodengsql/moodeng.toml

ENV MOODENG_DATA=/data
RUN sed -i 's|./moodeng_data|/data|g' /etc/moodengsql/moodeng.toml \
    && sed -i 's|127.0.0.1|0.0.0.0|g' /etc/moodengsql/moodeng.toml

VOLUME ["/data"]
EXPOSE 5432
WORKDIR /data

HEALTHCHECK --interval=10s --timeout=5s --retries=3 \
    CMD moodengsql ping --config /etc/moodengsql/moodeng.toml || exit 1

ENTRYPOINT ["moodengsql", "serve", "--config", "/etc/moodengsql/moodeng.toml"]
