# grpcknock

A small gRPC health check probe for the command line, a Rust alternative to
[grpc-health-probe](https://github.com/grpc-ecosystem/grpc-health-probe).
It calls the standard `grpc.health.v1.Health/Check` method and reports the
result through the process exit code, so it fits Kubernetes liveness and
readiness probes and Docker HEALTHCHECK lines.

## Installation

From crates.io (requires Rust 1.94 or newer):

```
cargo install --locked grpcknock
```

The latest unreleased state, straight from the repository:

```
cargo install --locked --git https://github.com/nullmonger/grpcknock
```

## Usage

Probe overall server health:

```
$ grpcknock --addr localhost:50051
status: SERVING
```

`--port N` is a shortcut for `--addr localhost:N`; exactly one of the two
is required. IPv6 hosts are bracketed: `--addr [::1]:50051`.

`--service` selects the service to check and can be repeated to check
several over one connection; the exit code then reflects the worst result.
Omit it to check overall health (the empty service name).

```
$ grpcknock --port 50051 --service demo.Serving --service demo.NotServing
demo.Serving: SERVING
demo.NotServing: NOT_SERVING
$ echo $?
3
```

### Exit codes

| Code | Meaning                                   |
|------|-------------------------------------------|
| 0    | SERVING                                   |
| 1    | connection failure (including TLS errors) |
| 2    | invocation error (the RPC itself failed)  |
| 3    | NOT_SERVING, UNKNOWN or SERVICE_UNKNOWN   |
| 4    | timeout (connection or request)           |

The mapping is deliberately not that of grpc-health-probe (which uses
1 for invalid arguments, 2 for connection failures, 3 for RPC failures,
4 for unhealthy, and has no timeout code): zero still means healthy, but
scripts keyed to specific non-zero codes need adjusting.

Invalid command-line arguments also exit with 2 (clap's default). An
unregistered service usually surfaces as a NOT_FOUND RPC error (exit 2);
the SERVICE_UNKNOWN status appears mainly in watch streams.

### Output formats

The exit code is the primary channel and never depends on the format;
stdout is controlled by mutually exclusive flags:

- default: `status: SERVING` on stdout, errors on stderr
- `--verbose` / `-v`: endpoint and service alongside the status
- `--json`: one machine-readable object, or an array when several services
  are checked
- `--quiet` / `-q`: no result output, exit code only

```
$ grpcknock --port 50051 --service demo.Serving --json
{"endpoint":"localhost:50051","service":"demo.Serving","status":"SERVING"}
```

### TLS

`--tls` connects over TLS using the system's trusted roots.
`--ca-cert <PATH>` verifies the server against a PEM CA certificate
instead, and `--tls-no-verify` skips certificate verification entirely
(debugging only); both imply `--tls`. Everything is built on rustls, so
no system OpenSSL is needed. Client certificates (mTLS) are not
supported; see [Limitations](#limitations).

### Watch mode

`--watch` streams `grpc.health.v1.Health/Watch` updates for a single
service (or overall health) instead of a one-shot check, printing each
update until the server closes the stream or Ctrl-C / SIGTERM arrives.
The exit code reflects the last observed status; a stream error exits
with 2, and a stream that ends before any update exits with 3. In watch
mode `--timeout` bounds opening the stream, not its lifetime, and
`--json` prints each update as a separate JSON object.

`--watch-failures N` makes the stream exit after N consecutive
non-serving updates; a return to SERVING resets the counter.

### Timeouts and retries

`--connect-timeout` limits connection establishment and `--timeout`
limits the health request. Both take a number with an explicit unit
(`500ms`, `2s`, `1m`) and map to exit code 4 when exceeded; without the
flags the probe waits indefinitely.

`--retry N` retries transient connection failures (refused, reset,
connect timeout) up to N times, one second apart. RPC errors and TLS
configuration errors are deterministic and are not retried; a TLS
handshake failure currently counts as a connection failure and is
retried.

### Metadata

`--metadata key=value` (alias: `--rpc-header`) attaches a gRPC metadata
header to every request; repeat the flag for several headers. Metadata
often carries credentials, so a warning is printed when it would travel
over a plaintext or unverified TLS connection. The warning goes to
stderr and is not suppressed by `--quiet`.

## Docker and Kubernetes

[examples/Dockerfile](examples/Dockerfile) builds a demo image where
grpcknock backs the `HEALTHCHECK`:

```dockerfile
HEALTHCHECK --interval=5s --timeout=4s --start-period=2s \
    CMD ["grpcknock", "--addr", "127.0.0.1:50051", "--service", "demo.Serving", \
         "--connect-timeout", "1s", "--timeout", "2s"]
```

[examples/k8s-probe.yaml](examples/k8s-probe.yaml) wires it into
Kubernetes exec probes:

```yaml
readinessProbe:
  exec:
    command: ["grpcknock", "--addr", "127.0.0.1:50051", "--service", "demo.Serving",
              "--connect-timeout", "1s", "--timeout", "2s"]
```

## Local development

The repository ships a mock health server that registers a handful of
service names with fixed statuses. Run it in one terminal:

```
cargo run --example mock_server
```

The server listens on `127.0.0.1:50051`. Probe it from another terminal
(use `cargo run --` instead of `grpcknock` when running from source):

```
grpcknock --port 50051                            # 0  SERVING (overall)
grpcknock --port 50051 --service demo.Serving     # 0  SERVING
grpcknock --port 50051 --service demo.NotServing  # 3  NOT_SERVING
grpcknock --port 50051 --service demo.Unknown     # 3  UNKNOWN
grpcknock --port 50051 --service demo.Missing     # 2  rpc NOT_FOUND
grpcknock --port 50052                            # 1  connection refused

grpcknock --port 50051 --service demo.Flapping --watch   # live updates
```

## Limitations

- No mTLS: `--client-cert` / `--client-key` are left for a later release.
- crates.io is the only distribution channel for now; prebuilt binaries
  and distro packages are deferred until the tool has seen real-world use.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE),
at your option.
