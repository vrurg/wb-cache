# check=skip=SecretsUsedInArgOrEnv
# Use the official Rust image as the builder
FROM rust:bookworm AS builder

ARG WBCACHE_FEATURES=""

WORKDIR /src

# Copy the entire project into the container
COPY . .

# This build argument permits passing custom compilation features (default empty)

RUN mkdir /src/install
RUN mkdir -p /usr/local/libexec
COPY ./docker/scripts/builder/* /usr/local/libexec/
RUN chmod +x /usr/local/libexec/*.sh
RUN /bin/sh -c "/usr/local/libexec/init.sh"

# Build the "company" example in release mode.
# If WBCACHE_FEATURES is provided, they will be passed to Cargo.
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-index,target=/usr/local/cargo/git \
    --mount=type=cache,id=src-cargo-target,target=/src/target \
    <<EOS
export CARGO_TARGET_DIR=/src/target/${WBCACHE_FEATURES}

if [ "$$WBCACHE_CLEAR_TARGET" = "true" ]; then
    echo "Clearing target directory..."
    cargo clean --target-dir /src/target/${WBCACHE_FEATURES}
fi

cargo install \
    --path /src \
    --root /src/install/${WBCACHE_FEATURES} \
    --target-dir /src/target/${WBCACHE_FEATURES} \
    --features "simulation" --features "$WBCACHE_FEATURES" \
    --example company
EOS

# Use a minimal base image for running the binary
FROM debian:bookworm-slim AS runtime

RUN apt-get update && \
    apt-get install -y libssl3 ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /wbcache

# Create a non-root user 'wbcache'
RUN useradd -m wbcache

RUN chown wbcache:wbcache /wbcache

###### SQLite Runtime Stage ######
FROM runtime AS runtime-sqlite

ARG WBCACHE_FEATURES=""

# Copy the compiled binary from the builder stage to /usr/local/bin
COPY --from=builder /src/install/${WBCACHE_FEATURES}/bin/company /usr/local/bin/company

RUN mkdir -p /usr/local/libexec/company-simulation-sqlite
COPY ./docker/scripts/company-simulation-sqlite/* /usr/local/libexec/company-simulation-sqlite/
RUN chmod +x /usr/local/libexec/company-simulation-sqlite/*

# Switch to non-root user
USER wbcache

CMD ["/bin/sh", "-c", "/usr/local/libexec/company-simulation-sqlite/init.sh" ]

###### PostgreSQL Runtime Stage ######
FROM runtime AS runtime-pg

ARG WBCACHE_FEATURES=""

RUN apt-get update && \
    apt-get install -y postgresql-client openssh-client sshpass && \
    rm -rf /var/lib/apt/lists/*

RUN mkdir -p /usr/local/libexec/company-simulation-pg

COPY ./docker/scripts/company-simulation-pg/* /usr/local/libexec/company-simulation-pg/
COPY ./docker/etc/ssh_config /etc/ssh/ssh_config

RUN chmod +x /usr/local/libexec/company-simulation-pg/*

# Copy the compiled binary from the builder stage to /usr/local/bin
COPY --from=builder /src/install/${WBCACHE_FEATURES}/bin/company /usr/local/bin/company

# Switch to non-root user
USER wbcache

ENTRYPOINT ["sh", "-c", "/usr/local/libexec/company-simulation-pg/init.sh"]