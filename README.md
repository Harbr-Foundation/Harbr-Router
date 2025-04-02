# Harbr-Router: High-Performance Rust Reverse Proxy with TCP Support

A blazingly fast, memory-efficient reverse proxy built in Rust using async I/O and designed for high-scale production workloads. Harbr-Router now supports both HTTP and raw TCP traffic, making it perfect for database proxying and other non-HTTP protocols.

## Features

- ⚡ **High Performance**: Built on Tokio and Hyper for maximum throughput
- 🔄 **Automatic Retries**: Configurable retry logic for failed requests
- ⏱️ **Smart Timeouts**: Per-route and global timeout configuration
- 🔍 **Health Checks**: Built-in health check support for upstreams
- 📊 **Metrics**: Prometheus-compatible metrics for monitoring
- 🔄 **Zero Downtime**: Graceful shutdown support
- 🛡️ **Battle-tested**: Built on production-grade libraries
- 🎯 **Path-based Routing**: Flexible route configuration
- 🌐 **Protocol Agnostic**: Support for both HTTP and TCP traffic
- 🗄️ **Database Support**: Automatic detection and handling of database protocols
- 🔌 **Connection Pooling**: Efficient reuse of TCP connections for better performance

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

# TCP Proxy Configuration
tcp_proxy:
  enabled: true
  listen_addr: "0.0.0.0:9090"
  connection_pooling: true
  max_idle_time_secs: 60

routes:
  # HTTP Routes
  "/api":
    upstream: "http://backend-api:8080"
    timeout_ms: 3000
    retry_count: 2
  
  # Database Route (automatically handled as TCP)
  "postgres-db":
    upstream: "postgresql://postgres-db:5432"
    is_tcp: true
    db_type: "postgresql"
    timeout_ms: 10000
```

3. Run the proxy:
```bash
harbr-router -c config.yml
```

## Configuration Reference

### Global Configuration

| Field | Type | Description | Default |
|-------|------|-------------|---------|
| `listen_addr` | String | Address and port to listen on for HTTP | Required |
| `global_timeout_ms` | Integer | Global request timeout in milliseconds | Required |
| `max_connections` | Integer | Maximum number of concurrent connections | Required |

### TCP Proxy Configuration

| Field | Type | Description | Default |
|-------|------|-------------|---------|
| `enabled` | Boolean | Enable TCP proxy functionality | `false` |
| `listen_addr` | String | Address and port for TCP listener | `0.0.0.0:9090` |
| `connection_pooling` | Boolean | Enable connection pooling for TCP | `true` |
| `max_idle_time_secs` | Integer | Max time to keep idle connections | `60` |

### HTTP Route Configuration

Each HTTP route is defined by a path prefix and its configuration:

| Field | Type | Description | Default |
|-------|------|-------------|---------|
| `upstream` | String | Upstream server URL | Required |
| `health_check_path` | String | Path for health checks | Optional |
| `timeout_ms` | Integer | Route-specific timeout in ms | Global timeout |
| `retry_count` | Integer | Number of retry attempts | 0 |

### TCP/Database Route Configuration

TCP routes use the same configuration structure with additional fields:

| Field | Type | Description | Default |
|-------|------|-------------|---------|
| `is_tcp` | Boolean | Mark route as TCP instead of HTTP | `false` |
| `db_type` | String | Database type (mysql, postgresql, etc.) | Optional |
| `tcp_listen_port` | Integer | Custom port for this TCP service | Optional |

### Example Configuration

```yaml
listen_addr: "0.0.0.0:8080"
global_timeout_ms: 5000
max_connections: 10000

tcp_proxy:
  enabled: true
  listen_addr: "0.0.0.0:9090"

routes:
  "/api":
    upstream: "http://backend-api:8080"
    health_check_path: "/health"
    timeout_ms: 3000
    retry_count: 2
  
  "/static":
    upstream: "http://static-server:80"
    timeout_ms: 1000
  
  "/":
    upstream: "http://default-backend:8080"
    timeout_ms: 2000
  
  # Database Routes
  "mysql-primary":
    upstream: "mysql://db-primary:3306"
    is_tcp: true
    db_type: "mysql"
    timeout_ms: 10000
    retry_count: 3
  
  "postgres-analytics":
    upstream: "postgresql://analytics-db:5432"
    is_tcp: true
    db_type: "postgresql"
    tcp_listen_port: 5433  # Custom listening port
```

## Database Support

Harbr-Router now includes automatic detection and support for common database protocols:

- **MySQL/MariaDB** (ports 3306, 33060)
- **PostgreSQL** (port 5432)
- **MongoDB** (ports 27017, 27018, 27019)
- **Redis** (port 6379)
- **Oracle** (port 1521)
- **SQL Server** (port 1433)
- **Cassandra** (port 9042)
- **CouchDB** (port 5984)
- **InfluxDB** (port 8086)
- **Elasticsearch** (ports 9200, 9300)

Database connections are automatically detected by:
1. Explicit configuration (`is_tcp: true` and `db_type: "..."`)
2. Port numbers in the upstream URL
3. Protocol prefixes (mysql://, postgresql://, etc.)

## TCP Proxy Operation

The TCP proxy operates by:

1. Accepting connections on the configured TCP listening port
2. Forwarding traffic to the appropriate upstream
3. Maintaining connection pooling for better performance
4. Applying timeouts and retries as configured

## Metrics

The proxy now exposes additional TCP proxy metrics at `/metrics`:

### Counter Metrics

| Metric | Labels | Description |
|--------|--------|-------------|
| `tcp_proxy.connection.new` | - | New TCP connections created |
| `tcp_proxy.connection.completed` | - | Completed TCP connections |
| `tcp_proxy.timeout` | - | TCP connection timeouts |

### Histogram Metrics

| Metric | Description |
|--------|-------------|
| `tcp_proxy.connection.duration_seconds` | TCP connection duration histogram |

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
      - "8080:8080"  # HTTP
      - "9090:9090"  # TCP
    volumes:
      - ./config.yml:/etc/harbr-router/config.yml
    command: ["-c", "/etc/harbr-router/config.yml"]
```

### Kubernetes

A ConfigMap example with both HTTP and TCP configuration:

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
    tcp_proxy:
      enabled: true
      listen_addr: "0.0.0.0:9090"
    routes:
      "/api":
        upstream: "http://backend-api:8080"
        health_check_path: "/health"
        timeout_ms: 3000
      "postgres-db":
        upstream: "postgresql://postgres-db:5432"
        is_tcp: true
```

## Performance Tuning

For high-performance deployments with TCP traffic, adjust system limits:

```bash
# /etc/sysctl.conf
net.core.somaxconn = 65535
net.ipv4.tcp_max_syn_backlog = 65535
net.ipv4.ip_local_port_range = 1024 65535
net.ipv4.tcp_tw_reuse = 1
fs.file-max = 2097152  # Increased for high TCP connection count
```

## Use Cases

- **Database Load Balancing**: Distribute database connections across multiple nodes
- **Database Connection Limiting**: Control the maximum connections to your database
- **Database Proxying**: Put your databases behind a secure proxy layer
- **Protocol Conversion**: Use as a bridge between different network protocols
- **Edge Proxy**: Use as an edge proxy for both HTTP and non-HTTP traffic
- **Multi-Protocol Gateway**: Handle mixed HTTP/TCP traffic at the edge

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