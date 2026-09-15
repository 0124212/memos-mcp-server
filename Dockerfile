FROM rust:1.88-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/memos-mcp-server /usr/local/bin/memos-mcp-server
EXPOSE 8080
ENV BIND=0.0.0.0:8080
ENV MEMOS_URL=http://memos:5230
ENTRYPOINT ["/usr/local/bin/memos-mcp-server"]
