# Harbr-Router: High-Performance Rust Reverse Proxy

A blazingly fast, memory-efficient reverse proxy built in Rust using async I/O and designed for high-scale production workloads.

## Features

- ⚡ **High Performance**: Built on Tokio and Hyper for maximum throughput
- 🔄 **Automatic Retries**: Configurable retry logic for failed requests
- ⏱️ **Smart Timeouts**: Per-route and global timeout configuration
- 🔍 **Health Checks**: Built-in health check support for upstreams
- 📊 **Metrics**: Prometheus-compatible metrics for monitoring
- 🔄 **Zero Downtime**: Graceful shutdown support
- 🛡️ **Battle-tested**: Built on production-grade libraries
- 🎯 **Path-based Routing**: Flexible route configuration

## Quick Start

1. Install using Cargo:
```bash
cargo install harbr-router
```

2. Create a configuration file `config.yml`:
```yaml
listen_addr: "0.0.0.0:8080"
global_timeout_ms: 5000
max_connections: 10000

routes:
  "/api":
    upstream: "http://backend-api:8080"
    health_check_path: "/health"
    timeout_ms: 3000
    retry_count: 2
```

3. Run the proxy:
```bash
harbr-router -c config.yml
```

## Configuration Reference

### Global Configuration

| Field | Type | Description | Default |
|-------|------|-------------|---------|
| `listen_addr` | String | Address and port to listen on | Required |
| `global_timeout_ms` | Integer | Global request timeout in milliseconds | Required |
| `max_connections` | Integer | Maximum number of concurrent connections | Required |

### Route Configuration

Each route is defined by a path prefix and its configuration:

| Field | Type | Description | Default |
|-------|------|-------------|---------|
| `upstream` | String | Upstream server URL | Required |
| `health_check_path` | String | Path for health checks | Optional |
| `timeout_ms` | Integer | Route-specific timeout in ms | Global timeout |
| `retry_count` | Integer | Number of retry attempts | 0 |

### Example Configuration

```yaml
listen_addr: "0.0.0.0:8080"
global_timeout_ms: 5000
max_connections: 10000

routes:
  "/api":
    upstream: "http://backend-api:8080"
    health_check_path: "/health"
    timeout_ms: 3000
    retry_count: 2
  
  "/static":
    upstream: "http://static-server:80"
    timeout_ms: 1000
    retry_count: 1
  
  "/":
    upstream: "http://default-backend:8080"
    timeout_ms: 2000
    retry_count: 2
```

### Route Matching

- Routes are matched by prefix
- More specific routes take precedence
- The "/" route acts as a catch-all default
- Health checks run on the specified path for each upstream

## Metrics

The proxy exposes Prometheus-compatible metrics at `/metrics`:

### Counter Metrics

| Metric | Labels | Description |
|--------|--------|-------------|
| `proxy_request_total` | `status=success\|error` | Total requests |
| `proxy_attempt_total` | `result=success\|failure\|timeout` | Request attempts |
| `proxy_timeout_total` | - | Total timeouts |

### Histogram Metrics

| Metric | Description |
|--------|-------------|
| `proxy_request_duration_seconds` | Request duration histogram |

## Health Checks

Health checks are performed on the specified `health_check_path` for each upstream:

- Method: GET
- Interval: 10 seconds
- Success: 200-299 status code
- Timeout: 5 seconds

Failed health checks will remove the upstream from the pool until it recovers.

## Production Deployment

### Docker

```dockerfile
FROM rust:1.70 as builder
WORKDIR /usr/src/harbr-router
COPY . .
RUN cargo build --release

FROM debian:bullseye-slim
COPY --from=builder /usr/src/harbr-router/target/release/harbr-router /usr/local/bin/
ENTRYPOINT ["harbr-router"]
```

### Docker Compose

```yaml
version: '3'
services:
  proxy:
    image: harbr-router:latest
    ports:
      - "8080:8080"
    volumes:
      - ./config.yml:/etc/harbr-router/config.yml
    command: ["-c", "/etc/harbr-router/config.yml"]
```

### Kubernetes

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: harbr-router-config
data:
  config.yml: |
    listen_addr: "0.0.0.0:8080"
    global_timeout_ms: 5000
    max_connections: 10000
    routes:
      "/api":
        upstream: "http://backend-api:8080"
        health_check_path: "/health"
        timeout_ms: 3000
        retry_count: 2

---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: harbr-router
spec:
  replicas: 3
  template:
    spec:
      containers:
      - name: harbr-router
        image: harbr-router:latest
        ports:
        - containerPort: 8080
        volumeMounts:
        - name: config
          mountPath: /etc/harbr-router
      volumes:
      - name: config
        configMap:
          name: harbr-router-config
```

## Performance Tuning

### System Limits

For high-performance deployments, adjust system limits:

```bash
# /etc/sysctl.conf
net.core.somaxconn = 65535
net.ipv4.tcp_max_syn_backlog = 65535
net.ipv4.ip_local_port_range = 1024 65535
net.ipv4.tcp_tw_reuse = 1
```

### Runtime Configuration

Set these environment variables for optimal performance:

```bash
export RUST_MIN_THREADS=4
export RUST_MAX_THREADS=32
```

## Contributing

Contributions are welcome! Please read our [Contributing Guide](CONTRIBUTING.md) for details on our code of conduct and the process for submitting pull requests.

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Acknowledgments

Built with:
- [Tokio](https://tokio.rs/) - Async runtime
- [Hyper](https://hyper.rs/) - HTTP implementation
- [Tower](https://github.com/tower-rs/tower) - Service abstractions
- [Metrics](https://metrics.rs/) - Metrics and monitoring