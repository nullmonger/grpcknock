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

## Status

Early development, pre-release. The command-line interface is not stable yet.

