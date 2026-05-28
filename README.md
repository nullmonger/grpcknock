# grpcknock

A small gRPC health check probe for the command line. It calls the standard
`grpc.health.v1.Health/Check` method and reports the result through the process
exit code, so it fits Kubernetes liveness and readiness probes and Docker
HEALTHCHECK lines.

grpcknock is a Rust alternative to grpc-health-probe.

## Installation

```
cargo install --git https://github.com/nullmonger/grpcknock
```

## Try it out locally

The repository ships a mock health server that registers a handful of service
names with fixed statuses. Run it in one terminal:

```bash
cargo run --example mock_server
```

The server listens on `127.0.0.1:50051`. Probe it from another terminal (use
`cargo run --` instead of `grpcknock` when running from source):

```bash
grpcknock --port 50051                            # 0  SERVING (overall)
grpcknock --port 50051 --service demo.Serving     # 0  SERVING
grpcknock --port 50051 --service demo.NotServing  # 3  NOT_SERVING
grpcknock --port 50051 --service demo.Unknown     # 3  UNKNOWN
grpcknock --port 50051 --service demo.Missing     # 2  rpc NOT_FOUND
grpcknock --port 50052                            # 1  connection refused
```

Exit codes follow grpc-health-probe: `0` SERVING, `1` connection error,
`2` invocation error, `3` NOT_SERVING or UNKNOWN.

## Status

Early development, pre-release. The command-line interface is not stable yet.

