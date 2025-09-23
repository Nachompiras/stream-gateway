# SRT Gateway

A high-performance **SRT (Secure Reliable Transport) Gateway** written in Rust that provides a REST API for managing real-time media streaming inputs and outputs. The gateway supports both UDP and SRT protocols and acts as a bridge between different streaming endpoints with enterprise-grade reliability and performance.

## 🚧 Work in Progress 🚧

This project is still in active development and subject to change.

## 🚀 Features

### **Protocol Support**
- **SRT Protocol**: Full support for SRT Listener and Caller modes
- **UDP Protocol**: High-performance UDP streaming
- **Bidirectional**: Both input (receive) and output (send) operations
- **Multi-stream**: Handle multiple concurrent streams efficiently

### **REST API Management**
- **Complete CRUD Operations** for inputs and outputs
- **Real-time Statistics** monitoring for all streams
- **Validation & Conflict Detection** for duplicate streams and port conflicts
- **Persistent Configuration** with SQLite database
- **JSON-based Configuration** for easy integration

### **Enterprise Features**
- **Async Architecture** using Tokio for high concurrency
- **Database Persistence** for configuration and state management
- **Automatic Reconnection** for SRT connections
- **Resource Management** with proper cleanup and error handling
- **Production Ready** with comprehensive error handling and logging

## 📋 API Endpoints

### **Inputs Management**
- `GET /inputs` - List all inputs
- `GET /inputs/{id}` - Get specific input details
- `POST /inputs` - Create new input
- `DELETE /inputs` - Delete input
- `GET /inputs/{id}/stats` - Get input statistics
- `GET /inputs/{input_id}/outputs` - List outputs for specific input

### **Outputs Management**
- `GET /outputs` - List all outputs
- `GET /outputs/{id}` - Get specific output details
- `POST /outputs` - Create new output
- `DELETE /outputs` - Delete output

### **System Status**
- `GET /status` - Complete system status with all inputs and outputs

## 🏗️ Architecture

The SRT Gateway is built with a modern, scalable architecture:

```
┌─────────────────┐    ┌──────────────────┐    ┌─────────────────┐
│   REST API      │    │   Stream Engine  │    │   Database      │
│   (Actix-Web)   │◄──►│   (Tokio Tasks)  │◄──►│   (SQLite)      │
└─────────────────┘    └──────────────────┘    └─────────────────┘
         │                        │                        │
         ▼                        ▼                        ▼
┌─────────────────┐    ┌──────────────────┐    ┌─────────────────┐
│   HTTP Client   │    │  SRT/UDP Streams │    │  Configuration  │
│   Integration   │    │  Broadcasting    │    │  Persistence    │
└─────────────────┘    └──────────────────┘    └─────────────────┘
```

### **Core Components**

- **HTTP API Layer**: RESTful interface built with Actix-Web
- **Stream Processing Engine**: Async stream handling with Tokio
- **Database Layer**: SQLite for configuration and state persistence
- **SRT Integration**: Custom Rust bindings to SRT library
- **Broadcast System**: Efficient packet distribution using Tokio channels

## 🚀 Quick Start

### **Prerequisites**
- Rust 1.70+ with Cargo
- SRT library installed system-wide (`libsrt`)
- SQLite (included)

### **Installation**
```bash
git clone https://github.com/Nachompiras/stream-gateway.git
cd stream-gateway
cargo build --release
```

### **Running the Gateway**
```bash
cargo run
```

The HTTP server will start on `http://127.0.0.1:8080`

### **Basic Usage Example**

1. **Create an SRT Listener Input**:
```bash
curl -X POST -H "Content-Type: application/json" \
  -d '{
    "type": "srt",
    "mode": "listener",
    "listen_port": 9000,
    "latency_ms": 120,
    "name": "TV Channel 1"
  }' http://127.0.0.1:8080/inputs
```

2. **Create a UDP Output for the Input**:
```bash
curl -X POST -H "Content-Type: application/json" \
  -d '{
    "type": "udp",
    "input_id": 1,
    "destination_addr": "127.0.0.1:5002",
    "name": "TV Channel 1 Backup"
  }' http://127.0.0.1:8080/outputs
```

3. **Check System Status**:
```bash
curl http://127.0.0.1:8080/status
```

## 🔧 Configuration

### **Environment Variables**
- `DATABASE_URL`: SQLite database path (default: `state.db`)
- `BIND_ADDRESS`: Server bind address (default: `127.0.0.1:8080`)
- `LOG_LEVEL`: Logging level (default: `info`)

### **SRT Configuration Options**
- `latency_ms`: SRT latency in milliseconds
- `stream_id`: SRT stream identifier
- `passphrase`: SRT encryption passphrase
- `listen_port`: Port for SRT listener mode
- `target_addr`: Target address for SRT caller mode

## 📊 Stream Types

### **Input Types** (Data Receivers)
- **UDP Listener**: Receives UDP packets on specified port
- **SRT Listener**: Accepts SRT connections on specified port
- **SRT Caller**: Connects to remote SRT endpoint

### **Output Types** (Data Senders)
- **UDP Output**: Sends packets to UDP destination
- **SRT Caller Output**: Sends packets via SRT connection
- **SRT Listener Output**: Accepts SRT connections and sends packets

## 📈 Monitoring & Statistics

Real-time statistics are available for all inputs:

```bash
curl http://127.0.0.1:8080/inputs/1/stats
```

**Available Metrics**:
- Packet counts and rates
- Bitrate measurements
- Connection status
- Error counts
- Latency information (SRT)

## 🚧 Roadmap & TODO

### **Prometheus Metrics** 🎯
- Export input/output statistics to Prometheus format
- Performance metrics (latency, throughput, error rates)
- Health monitoring endpoints (`/metrics`)
- Grafana dashboard templates
- SLA monitoring and alerting

### **Events Webhooks** 🔗
- Stream start/stop notifications
- Error event notifications with severity levels
- Health status change events
- Connection state changes
- Custom webhook URL configurations
- Retry mechanisms and delivery guarantees

### **TR 101 290 Support** 📺
- **MPEG-TS Analysis**: Real-time transport stream analysis
- **PSI/SI Table Monitoring**: Program information analysis
- **Stream Continuity Checking**: Detect discontinuities and errors
- **ETR 101 290 Compliance**: Full compliance with ETSI standards
- **Broadcasting Metrics**: Industry-standard monitoring for broadcast workflows
- **Transport Stream Validation**: Comprehensive TS stream validation

## 🛠️ Development

### **Building from Source**
```bash
# Debug build
cargo build

# Release build
cargo build --release

# Run tests
cargo test

# Run with logging
RUST_LOG=debug cargo run
```

### **Code Quality**
```bash
# Format code
cargo fmt

# Run linter
cargo clippy

# Check for security issues
cargo audit
```

## 🏷️ Custom Naming

All inputs and outputs support optional custom naming for better organization:

### **Named UDP Input Example**
```bash
curl -X POST -H "Content-Type: application/json" \
  -d '{
    "type": "udp",
    "listen_port": 5000,
    "name": "TV mas"
  }' http://127.0.0.1:8080/inputs
```

### **Named Outputs Example**
```bash
# Primary output
curl -X POST -H "Content-Type: application/json" \
  -d '{
    "type": "udp",
    "input_id": 1,
    "destination_addr": "127.0.0.1:8001",
    "name": "TV mas principal"
  }' http://127.0.0.1:8080/outputs

# Backup output
curl -X POST -H "Content-Type: application/json" \
  -d '{
    "type": "udp",
    "input_id": 1,
    "destination_addr": "127.0.0.1:8002",
    "name": "TV mas backup"
  }' http://127.0.0.1:8080/outputs
```

When you list inputs/outputs, they'll show with your custom names:
```bash
curl http://127.0.0.1:8080/status
```

**Response Example:**
```json
[
  {
    "id": 1,
    "name": "TV mas",
    "details": "UDP Listener on port 5000",
    "outputs": [
      {
        "id": 1,
        "name": "TV mas principal",
        "input_id": 1,
        "destination": "127.0.0.1:8001",
        "output_type": "udp"
      },
      {
        "id": 2,
        "name": "TV mas backup",
        "input_id": 1,
        "destination": "127.0.0.1:8002",
        "output_type": "udp"
      }
    ]
  }
]
```

> **Note**: Names are optional. If not provided, the system will auto-generate descriptive names like "UDP Listener 5000" or "SRT Caller to 192.168.1.100:9000".

## 📝 API Examples

### **Create SRT Caller Input**
```bash
curl -X POST -H "Content-Type: application/json" \
  -d '{
    "type": "srt",
    "mode": "caller",
    "target_addr": "192.168.1.100:9000",
    "stream_id": "live_stream",
    "latency_ms": 200,
    "passphrase": "secret_key",
    "name": "Remote Studio Feed"
  }' http://127.0.0.1:8080/inputs
```

### **Create SRT Listener Output**
```bash
curl -X POST -H "Content-Type: application/json" \
  -d '{
    "type": "srt",
    "mode": "listener",
    "input_id": 1,
    "listen_port": 8000,
    "latency_ms": 120,
    "name": "Primary Distribution"
  }' http://127.0.0.1:8080/outputs
```

### **List All Inputs**
```bash
curl http://127.0.0.1:8080/inputs
```

### **Delete Output**
```bash
curl -X DELETE -H "Content-Type: application/json" \
  -d '{
    "input_id": 1,
    "output_id": 2
  }' http://127.0.0.1:8080/outputs
```

## 🤝 Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Make your changes
4. Run tests and linting
5. Commit your changes (`git commit -m 'Add amazing feature'`)
6. Push to the branch (`git push origin feature/amazing-feature`)
7. Open a Pull Request

## 📄 License

This project is licensed under the Apache License - see the [LICENSE](LICENSE) file for details.

## 🙋 Support

- **Documentation**: Check this README and inline code documentation
- **Issues**: Report bugs and request features via GitHub Issues
- **Discussions**: Join discussions for questions and ideas

## 🏢 Enterprise Use Cases

- **Live Streaming Infrastructure**: High-performance streaming gateway
- **Broadcast Workflows**: Professional media transport and distribution
- **Content Delivery Networks**: Reliable media relay and distribution
- **Remote Production**: Secure media transport over unreliable networks
- **Monitoring Systems**: Stream health monitoring and statistics
- **Media Processing Pipelines**: Integration point for media workflows

---

Built with ❤️ in Rust for high-performance media streaming applications.