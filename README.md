# Harbr-Router: High-Performance Universal Proxy

A blazingly fast, memory-efficient multi-protocol proxy built in Rust using async I/O and designed for high-scale production workloads. Harbr-Router supports HTTP, TCP, and UDP traffic through a unified configuration interface.

## Features

- ⚡ **High Performance**: Built on Tokio and Hyper for maximum throughput
- 🔄 **Automatic Retries**: Configurable retry logic for failed requests
- ⏱️ **Smart Timeouts**: Per-route and global timeout configuration
- 📊 **Metrics**: Prometheus-compatible metrics for monitoring
- 🔄 **Zero Downtime**: Graceful shutdown and dynamic configuration support
- 🛡️ **Battle-tested**: Built on production-grade libraries
- 🌐 **Protocol Agnostic**: Support for HTTP, TCP, and UDP traffic
- 🗄️ **Database Support**: Automatic detection and handling of database protocols
- 🔌 **Connection Pooling**: Efficient reuse of TCP connections for better performance
- 🔄 **UDP Proxying**: Support for stateless UDP protocols (DNS, syslog, game servers)
- 🎯 **Path-based Routing**: Flexible route configuration for HTTP
- 🔍 **Health Checks**: Built-in health check support for HTTP upstreams
- 📚 **Library API**: Use as a standalone binary or integrate as a library in your Rust applications
- 🔁 **Dynamic Configuration**: Update routes and settings at runtime without restarts

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

# TCP/UDP Proxy Configuration
tcp_proxy:
  enabled: true
  listen_addr: "0.0.0.0:9090"
  connection_pooling: true
  max_idle_time_secs: 60
  udp_enabled: true
  udp_listen_addr: "0.0.0.0:9090"  # Same port as TCP

routes:
  # HTTP Routes
  "/api":
    upstream: "http://backend-api:8080"
    timeout_ms: 3000
    retry_count: 2
  
  # Default HTTP route
  "/":
    upstream: "http://default-backend:8080"
    timeout_ms: 5000
    retry_count: 1
  
  # Database Route (automatically handled as TCP)
  "postgres-db":
    upstream: "postgresql://postgres-db:5432"
    is_tcp: true
    db_type: "postgresql"
    timeout_ms: 10000
    
  # UDP Route
  "dns-service":
    upstream: "dns-server:53"
    is_udp: true
    timeout_ms: 1000
```

3. Run the proxy:
```bash
harbr-router -c config.yml
```

4. Enable dynamic configuration (optional):
```bash
harbr-router -c config.yml --enable-api --watch-config
```

## Using Harbr-Router as a Library

Harbr-Router can be used as a library in your Rust applications, allowing you to embed proxy functionality directly without external configuration files.

### Add to Your Project

Add Harbr-Router to your `Cargo.toml`:

```toml
[dependencies]
harbr_router = "0.1.0"
```

### Example: Creating a Router Programmatically

```rust
use harbr_router::{Router, ProxyConfig, RouteConfig, TcpProxyConfig};
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize the configuration programmatically
    let config = ProxyConfig::new("0.0.0.0:8080", 30000, 1000)
        // Add HTTP routes
        .with_route("api", 
            RouteConfig::new("http://api-backend:8000")
                .with_timeout(5000)
                .with_retry_count(3)
                .preserve_host_header(true)
        )
        .with_route("app", 
            RouteConfig::new("http://web-app:3000")
                .with_priority(10)
        )
        // Add database route (automatically handled as TCP)
        .with_route("postgres", 
            RouteConfig::new("postgresql://db.example.com:5432")
                .with_db_type("postgresql")
        )
        // Add UDP route for metrics
        .with_route("metrics", 
            RouteConfig::new("metrics.internal:8125")
                .as_udp(true)
                .with_udp_listen_port(8125)
        )
        // Configure TCP and UDP proxy settings
        .enable_tcp_proxy(true)
        .tcp_listen_addr("0.0.0.0:9090")
        .enable_udp_proxy(true)
        .udp_listen_addr("0.0.0.0:9091");

    // Create and start the router
    let router = Router::new(config);
    router.start().await?;

    Ok(())
}
```

### Example: Loading Configuration from a File

```rust
use harbr_router::Router;
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Load configuration from a file
    let router = Router::from_file("config.yml").await?;
    
    // Start the router
    router.start().await?;

    Ok(())
}
```

### Example: Dynamic Configuration Updates

```rust
use harbr_router::{Router, RouteConfig};
use anyhow::Result;
use std::sync::Arc;
use tokio::time::sleep;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    // Load configuration from a file
    let router = Router::from_file("config.yml").await?;
    
    // Start the router in a separate task
    let router_clone = router.clone();
    tokio::spawn(async move {
        router_clone.start().await.unwrap();
    });
    
    // Wait for the router to initialize
    sleep(Duration::from_secs(1)).await;
    
    // Update an existing route or add a new one at runtime
    let updated_api_route = RouteConfig::new("http://new-api-backend:8080")
        .with_timeout(2000)
        .with_retry_count(3)
        .with_priority(100);
    
    router.update_route("/api", updated_api_route).await?;
    println!("Updated /api route with zero downtime");
    
    // Add a new route at runtime
    let metrics_route = RouteConfig::new("http://metrics-server:9091")
        .with_timeout(1000);
    
    router.add_route("/metrics", metrics_route).await?;
    println!("Added /metrics route dynamically");

    Ok(())
}
```

## Dynamic Configuration

Harbr-Router supports real-time configuration changes without requiring restarts or causing service interruptions. Configuration can be updated through:

1. **Programmatic API**: Direct method calls on the Router struct
2. **HTTP API**: RESTful API for remote management
3. **File Watching**: Automatic detection of configuration file changes
4. **CLI Tool**: Command-line interface for administrative tasks

### Using the HTTP API

Enable the API when starting the router:

```bash
harbr-router -c config.yml --enable-api --api-port 8082
```

Example API calls:

```bash
# Get current configuration
curl http://localhost:8082/api/config

# Add or update a route
curl -X PUT http://localhost:8082/api/config/routes/api \
    -H "Content-Type: application/json" \
    -d '{"upstream":"http://new-backend:8080", "timeout_ms":3000}'

# Delete a route
curl -X DELETE http://localhost:8082/api/config/routes/api

# Update TCP configuration
curl -X PUT http://localhost:8082/api/config/tcp \
    -H "Content-Type: application/json" \
    -d '{"enabled":true, "listen_addr":"0.0.0.0:9090"}'

# Update global settings
curl -X PUT http://localhost:8082/api/config/global \
    -H "Content-Type: application/json" \
    -d '{"global_timeout_ms":10000}'

# Reload configuration from file
curl -X POST http://localhost:8082/api/config/reload
```

### Using the CLI Tool

The harbr-cli tool provides a simple command-line interface for configuration management:

```bash
# Install the CLI tool
cargo install harbr-cli

# Add or update a route
harbr-cli update route /api --upstream http://backend:8080 --timeout 3000

# Delete a route
harbr-cli delete route /api

# Update TCP config
harbr-cli update tcp --enabled true --listen-addr 0.0.0.0:9090

# Update global settings
harbr-cli update global --timeout 10000

# View current configuration
harbr-cli get config
```

### Automatic File Watching

Enable automatic file watching to have Harbr-Router detect and apply configuration changes when the config file is modified:

```bash
harbr-router -c config.yml --watch-config --watch-interval 30
```

## Library API Reference

The Harbr-Router library provides a fluent builder-style API for configuration:

#### Router

The main struct that manages all proxy services:

```rust
// Create a new router with programmatic config
let router = Router::new(config);

// Create a router from config file
let router = Router::from_file("config.yml").await?;

// Start all enabled proxies
router.start().await?;

// Dynamic configuration updates
router.add_route("/new-route", route_config).await?;
router.update_route("/existing-route", updated_config).await?;
router.remove_route("/old-route").await?;
router.update_tcp_config(tcp_config).await?;
router.update_global_settings(Some("0.0.0.0:8081"), Some(10000), Some(20000)).await?;
router.reload_config().await?;
```

#### ProxyConfig

Configuration for the entire proxy system:

```rust
// Create a new configuration
let config = ProxyConfig::new(
    "0.0.0.0:8080",  // HTTP listen address
    30000,           // Global timeout in milliseconds
    1000             // Max connections
);

// Add a route
config = config.with_route("name", route_config);

// Configure TCP proxy
config = config
    .enable_tcp_proxy(true)
    .tcp_listen_addr("0.0.0.0:9090");
    
// Configure UDP proxy
config = config
    .enable_udp_proxy(true)
    .udp_listen_addr("0.0.0.0:9091");
```

#### RouteConfig

Configuration for individual routes:

```rust
// Create a new route
let route = RouteConfig::new("http://backend:8080")
    .with_timeout(5000)              // Route-specific timeout
    .with_retry_count(3)             // Number of retries
    .with_priority(10)               // Route priority
    .preserve_host_header(true);     // Preserve original host header

// TCP-specific configuration
let tcp_route = RouteConfig::new("db.example.com:5432")
    .as_tcp(true)                    // Mark as TCP route
    .with_tcp_listen_port(5432)      // Custom listen port
    .with_db_type("postgresql");     // Database type
    
// UDP-specific configuration
let udp_route = RouteConfig::new("metrics.internal:8125")
    .as_udp(true)                    // Mark as UDP route
    .with_udp_listen_port(8125);     // Custom listen port
```

## Configuration Reference

### Global Configuration

| Field | Type | Description | Default |
|-------|------|-------------|---------|
| `listen_addr` | String | Address and port to listen on for HTTP | Required |
| `global_timeout_ms` | Integer | Global request timeout in milliseconds | Required |
| `max_connections` | Integer | Maximum number of concurrent connections | Required |

### TCP/UDP Proxy Configuration

| Field | Type | Description | Default |
|-------|------|-------------|---------|
| `enabled` | Boolean | Enable TCP proxy functionality | `false` |
| `listen_addr` | String | Address and port for TCP listener | `0.0.0.0:9090` |
| `connection_pooling` | Boolean | Enable connection pooling for TCP | `true` |
| `max_idle_time_secs` | Integer | Max time to keep idle connections | `60` |
| `udp_enabled` | Boolean | Enable UDP proxy functionality | `false` |
| `udp_listen_addr` | String | Address and port for UDP listener | Same as TCP |

### HTTP Route Configuration

Each HTTP route is defined by a path prefix and its configuration:

| Field | Type | Description | Default |
|-------|------|-------------|---------|
| `upstream` | String | Upstream server URL | Required |
| `health_check_path` | String | Path for health checks | Optional |
| `timeout_ms` | Integer | Route-specific timeout in ms | Global timeout |
| `retry_count` | Integer | Number of retry attempts | 0 |
| `priority` | Integer | Route priority (higher wins) | 0 |
| `preserve_host_header` | Boolean | Preserve original Host header | `false` |

### TCP/UDP/Database Route Configuration

Non-HTTP routes use the same configuration structure with additional fields:

| Field | Type | Description | Default |
|-------|------|-------------|---------|
| `is_tcp` | Boolean | Mark route as TCP instead of HTTP | `false` |
| `is_udp` | Boolean | Mark route as UDP instead of TCP/HTTP | `false` |
| `db_type` | String | Database type (mysql, postgresql, etc.) | Optional |
| `tcp_listen_port` | Integer | Custom port for this TCP service | Optional |
| `udp_listen_port` | Integer | Custom port for this UDP service | Optional |

### Example Configuration

```yaml
listen_addr: "0.0.0.0:8080"
global_timeout_ms: 5000
max_connections: 10000

tcp_proxy:
  enabled: true
  listen_addr: "0.0.0.0:9090"
  udp_enabled: true
  udp_listen_addr: "0.0.0.0:9090"  # Same port as TCP

routes:
  # HTTP Routes
  "/api/critical":
    upstream: "http://critical-backend:8080"
    priority: 100
    timeout_ms: 1000
    retry_count: 3
  
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
    
  # UDP Routes
  "dns-service":
    upstream: "dns-server:53"
    is_udp: true
    timeout_ms: 1000
  
  "syslog-collector":
    upstream: "logging-service:514"
    is_udp: true
    timeout_ms: 2000
```

## Database Support

Harbr-Router automatically detects and supports common database protocols:

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

## UDP Protocol Support

Harbr-Router provides first-class support for UDP-based protocols:

- **DNS** (port 53)
- **Syslog** (port 514)
- **SNMP** (port 161)
- **NTP** (port 123)
- **Game server protocols**
- **Custom UDP services**

UDP connections are explicitly configured with:
- `is_udp: true` in the route configuration
- Setting the upstream destination in the standard format

## HTTP Route Matching

- Routes are matched by prefix (most specific wins)
- Priority can be explicitly set (higher number = higher priority)
- More specific routes take precedence when priority is equal
- The "/" route acts as a catch-all default

## TCP Proxy Operation

The TCP proxy operates by:

1. Accepting connections on the configured TCP listening port
2. Forwarding traffic to the appropriate upstream
3. Maintaining connection pooling for better performance
4. Applying timeouts and retries as configured

## UDP Proxy Operation

The UDP proxy operates by:

1. Receiving datagrams on the configured UDP listening port
2. Determining the appropriate upstream based on client address or first packet
3. Forwarding datagrams to the upstream destination
4. Relaying responses back to the original client

## Metrics

The proxy exposes Prometheus-compatible metrics at `/metrics`:

### Counter Metrics

| Metric | Labels | Description |
|--------|--------|-------------|
| `proxy_request_total` | `status=success\|error` | HTTP requests total |
| `proxy_attempt_total` | `result=success\|failure\|timeout` | HTTP request attempts |
| `proxy_timeout_total` | - | HTTP timeouts |
| `tcp_proxy.connection.new` | - | New TCP connections created |
| `tcp_proxy.connection.completed` | - | Completed TCP connections |
| `tcp_proxy.timeout` | - | TCP connection timeouts |
| `udp_proxy.datagram.received` | - | UDP datagrams received |
| `udp_proxy.datagram.forwarded` | - | UDP datagrams forwarded |
| `udp_proxy.datagram.response_sent` | - | UDP responses sent back to clients |
| `udp_proxy.timeout` | - | UDP timeout counter |
| `udp_proxy.unexpected_source` | - | Responses from unexpected sources |

### Histogram Metrics

| Metric | Description |
|--------|-------------|
| `proxy_request_duration_seconds` | HTTP request duration histogram |
| `tcp_proxy.connection.duration_seconds` | TCP connection duration histogram |
| `udp_proxy.datagram.duration` | UDP transaction duration histogram |

## Production Deployment

### Docker

```dockerfile
FROM rust:1.70 as builder
WORKDIR /usr/src/harbr-router
COPY . .
RUN cargo build --release

FROM debian:bullseye-slim
RUN apt-get update && apt-get install -y ca-certificates tzdata && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/src/harbr-router/target/release/harbr-router /usr/local/bin/
RUN mkdir -p /etc/harbr-router
RUN useradd -r -U -s /bin/false harbr && chown -R harbr:harbr /etc/harbr-router
USER harbr
WORKDIR /etc/harbr-router
ENV CONFIG_FILE="/etc/harbr-router/config.yml"
EXPOSE 8080 9090 8082
ENTRYPOINT ["harbr-router"]
CMD ["-c", "/etc/harbr-router/config.yml", "--enable-api", "--watch-config"]
```

### Docker Compose

```yaml
version: '3'
services:
  proxy:
    image: harbr-router:latest
    ports:
      - "8080:8080"  # HTTP
      - "9090:9090/tcp"  # TCP
      - "9090:9090/udp"  # UDP
      - "8082:8082"  # Config API (optional)
    volumes:
      - ./config.yml:/etc/harbr-router/config.yml
    environment:
      - RUST_LOG=info
    command: ["-c", "/etc/harbr-router/config.yml", "--enable-api", "--watch-config"]
```

### Kubernetes

A ConfigMap example with HTTP, TCP, and UDP configuration:

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
      udp_enabled: true
      udp_listen_addr: "0.0.0.0:9090"
    routes:
      "/api":
        upstream: "http://backend-api:8080"
        health_check_path: "/health"
        timeout_ms: 3000
      "postgres-db":
        upstream: "postgresql://postgres-db:5432"
        is_tcp: true
      "dns-service":
        upstream: "kube-dns.kube-system:53"
        is_udp: true
```

## Zero Downtime Operations

Harbr-Router is designed for zero downtime operations:

1. **Dynamic Route Updates**: Add, modify, or remove routes without restarting
2. **Graceful Shutdowns**: Complete in-flight requests before shutting down
3. **Live Reloading**: Automatic application of configuration changes
4. **Connection Handling**: Preserve existing connections while updating routes
5. **Blue/Green Deployments**: Gradually shift traffic between backends

Zero downtime operations are supported for:
- Adding new routes
- Changing upstream destinations
- Modifying timeouts, retries, and priorities
- Updating TCP/UDP configuration
- Adjusting global settings

## Performance Tuning

For high-performance deployments with TCP and UDP traffic, adjust system limits:

```bash
# /etc/sysctl.conf
net.core.somaxconn = 65535
net.ipv4.tcp_max_syn_backlog = 65535
net.ipv4.ip_local_port_range = 1024 65535
net.ipv4.tcp_tw_reuse = 1
fs.file-max = 2097152  # Increased for high connection count
net.core.rmem_max = 26214400  # Increase UDP receive buffer
net.core.wmem_max = 26214400  # Increase UDP send buffer
```

## Use Cases

- **Multi-Protocol API Gateway**: Handle HTTP, TCP, and UDP services with a single proxy
- **Database Connection Management**: Control database connections with pooling and timeouts
- **Microservice Architecture**: Route traffic between internal services
- **Edge Proxy**: Use as an edge proxy for all protocols
- **IoT Gateway**: Handle diverse protocols used by IoT devices
- **Game Server Infrastructure**: Proxy both TCP and UDP game traffic
- **Embedded Proxy**: Integrate proxy functionality directly into your Rust applications
- **Dynamic Routing**: Update routing rules at runtime for flexible traffic management

## Contributing

Contributions are welcome!

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Acknowledgments

Built with:
- [Tokio](https://tokio.rs/) - Async runtime
- [Hyper](https://hyper.rs/) - HTTP implementation
- [Tower](https://github.com/tower-rs/tower) - Service abstractions
- [Metrics](https://metrics.rs/) - Metrics and monitoring
- [Warp](https://github.com/seanmonstar/warp) - Web framework for API