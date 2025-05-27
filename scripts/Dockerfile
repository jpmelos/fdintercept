FROM rust:1.87-slim

RUN apt update && \
    apt install -y pkg-config libssl-dev && \
    rm -rf /var/lib/apt/lists/*

RUN cargo install cargo-tarpaulin

# Create and set permissions for cargo directory. This is required by
# tarpaulin.
RUN mkdir -p /usr/local/cargo && \
    chmod -R 777 /usr/local/cargo

WORKDIR /fdintercept
