# Changelog

All notable changes to this project are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-06-11

### Added

- One-shot `grpc.health.v1.Health/Check` probe with the result reported
  through the process exit code: 0 SERVING, 1 connection failure,
  2 invocation error, 3 non-serving status, 4 timeout.
- Addressing via `--addr host:port` (IPv6 in brackets) or `--port N` as a
  shortcut for `localhost:N`.
- Per-service checks with `--service`, repeatable to check several services
  over one connection; the exit code reflects the worst result.
- Output formats: plain status line by default, `--verbose`, `--json`
  (object, or array for several services) and `--quiet`.
- TLS support on rustls: `--tls` (system roots), `--ca-cert` (custom PEM CA)
  and `--tls-no-verify` (debugging only).
- Watch mode: `--watch` streams `Health/Watch` updates; `--watch-failures N`
  exits after N consecutive non-serving updates.
- Timeouts and retries: `--connect-timeout`, `--timeout`, `--retry N` for
  transient connection failures.
- gRPC metadata headers with `--metadata key=value` (alias `--rpc-header`),
  with a warning when metadata travels over a plaintext or unverified TLS
  connection.
- Examples: a multi-stage Dockerfile wiring grpcknock into a HEALTHCHECK
  and a Kubernetes Pod manifest with exec readiness and liveness probes.

[Unreleased]: https://github.com/nullmonger/grpcknock/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/nullmonger/grpcknock/releases/tag/v0.1.0
