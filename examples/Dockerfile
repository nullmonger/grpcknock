# Demonstrates wiring grpcknock into a Docker HEALTHCHECK.
#
# The demo application is the mock health server from this repository;
# in your own image, replace the mock_server build with your service build
# and keep the `cargo install grpcknock` and HEALTHCHECK lines.
#
# Build and watch the health status turn green:
#
#   docker build -f examples/Dockerfile -t grpcknock-demo .
#   docker run -d --name grpcknock-demo grpcknock-demo
#   docker inspect --format '{{.State.Health.Status}}' grpcknock-demo

FROM rust:1-slim-bookworm AS builder

# The probe itself comes from crates.io.
RUN cargo install --locked grpcknock

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY examples examples
RUN cargo build --locked --release --example mock_server

# The same Debian release as the builder: the binaries need its glibc.
FROM debian:bookworm-slim

COPY --from=builder /usr/local/cargo/bin/grpcknock /usr/local/bin/grpcknock
COPY --from=builder /src/target/release/examples/mock_server /usr/local/bin/mock_server

# Neither binary needs privileges; the healthcheck runs as the same user.
RUN useradd --system --no-create-home app
USER app

# No EXPOSE: the mock server binds to 127.0.0.1; the image demonstrates
# the HEALTHCHECK, not serving traffic.
#
# Docker documents only 0 as healthy and 1 as unhealthy, but in practice
# any non-zero status counts, so grpcknock's 1-4 all read as unhealthy.
# The 4s timeout leaves headroom over the probe's budget (1s + 2s),
# so a timeout exits as code 4 instead of being killed mid-flight.
HEALTHCHECK --interval=5s --timeout=4s --start-period=2s \
    CMD ["grpcknock", "--addr", "127.0.0.1:50051", "--service", "demo.Serving", \
         "--connect-timeout", "1s", "--timeout", "2s"]

CMD ["mock_server"]
