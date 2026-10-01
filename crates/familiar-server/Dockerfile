# Build from the repo root:  docker build -f crates/familiar-server/Dockerfile .
FROM rust:1-slim AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config build-essential cmake && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release -p familiar-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/familiar-server /usr/local/bin/familiar-server
ENV PORT=8080
EXPOSE 8080
CMD ["familiar-server"]
