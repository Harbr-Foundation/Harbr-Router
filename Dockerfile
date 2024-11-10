FROM rust:1.70 as builder
WORKDIR /usr/src/harbr-router

# Create blank project
RUN USER=root cargo new harbr-router
WORKDIR /usr/src/harbr-router/harbr-router

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Copy source code
COPY src src/

# Build for release
RUN cargo build --release

# Runtime stage
FROM debian:bullseye-slim

# Install necessary certificates and timezone data
RUN apt-get update && apt-get install -y \
    ca-certificates \
    tzdata \
    && rm -rf /var/lib/apt/lists/*

# Copy our build
COPY --from=builder /usr/src/harbr-router/harbr-router/target/release/harbr-router /usr/local/bin/

# Create directory for config
RUN mkdir -p /etc/harbr-router

# Create non-root user
RUN useradd -r -U -s /bin/false harbr && \
    chown -R harbr:harbr /etc/harbr-router

USER harbr
WORKDIR /etc/harbr-router

# Default config file location
ENV CONFIG_FILE="/etc/harbr-router/config.yml"

# Expose metrics port
EXPOSE 8080

ENTRYPOINT ["harbr-router"]
CMD ["-c", "/etc/harbr-router/config.yml"]