//! Pure data model for the container manager.
//!
//! Everything here is rendering- and IO-free: the [`podman`](crate::podman)
//! layer fills these structs from the live engine (or from the bundled
//! sample set when no engine is reachable), and the widget layer renders
//! them. Keeping the model pure means the list/detail logic is unit-testable
//! without a running daemon.

use serde::{Deserialize, Serialize};

/// First up to 12 characters of `id`, stopping at a `char` boundary. Podman
/// ids are hex digests (always ASCII), but this walks `char_indices` rather
/// than byte-slicing so it can never panic on non-ASCII input.
fn short_id_of(id: &str) -> &str {
    let end = id.char_indices().nth(12).map_or(id.len(), |(idx, _)| idx);
    id.get(..end).unwrap_or_default()
}

/// Lifecycle state of a container, normalised across the various strings
/// podman reports (`running`, `exited`, `created`, `paused`, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContainerStatus {
    Running,
    Exited,
    Created,
    Paused,
    Restarting,
    Dead,
}

impl ContainerStatus {
    /// Parse podman's `State`/`Status` string into a normalised status.
    /// Unknown values fall back to [`ContainerStatus::Dead`] so they read
    /// as "needs attention" rather than silently as "running".
    pub fn parse(raw: &str) -> Self {
        let lower = raw.trim().to_ascii_lowercase();
        if lower.starts_with("up") || lower.starts_with("running") {
            ContainerStatus::Running
        } else if lower.starts_with("exited") || lower.starts_with("stopped") {
            ContainerStatus::Exited
        } else if lower.starts_with("created") {
            ContainerStatus::Created
        } else if lower.starts_with("paused") {
            ContainerStatus::Paused
        } else if lower.starts_with("restarting") {
            ContainerStatus::Restarting
        } else {
            ContainerStatus::Dead
        }
    }

    /// Short human label used in the status pill.
    pub fn label(self) -> &'static str {
        match self {
            ContainerStatus::Running => "Running",
            ContainerStatus::Exited => "Exited",
            ContainerStatus::Created => "Created",
            ContainerStatus::Paused => "Paused",
            ContainerStatus::Restarting => "Restarting",
            ContainerStatus::Dead => "Dead",
        }
    }

    pub fn is_running(self) -> bool {
        matches!(self, ContainerStatus::Running | ContainerStatus::Restarting)
    }
}

/// A published port mapping (host → container).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortMapping {
    pub host_port: u16,
    pub container_port: u16,
    pub protocol: String,
}

/// A bind/volume mount of the container.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    pub source: String,
    pub destination: String,
    pub read_only: bool,
}

/// Live resource sample for a running container. Values are best-effort —
/// stopped containers report zeros.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    /// True only when a telemetry sample was actually read. Missing telemetry
    /// is not a measured zero (for example after an engine connection drops).
    #[serde(default)]
    pub available: bool,
    /// CPU usage as a fraction of one core, 0.0..=N (N = cores).
    pub cpu_percent: f32,
    /// Resident memory in bytes.
    pub mem_bytes: u64,
    /// Memory limit in bytes (0 = unlimited).
    pub mem_limit_bytes: u64,
    /// Cumulative network received bytes.
    pub net_rx_bytes: u64,
    /// Cumulative network transmitted bytes.
    pub net_tx_bytes: u64,
    /// Block-device read bytes.
    pub block_read_bytes: u64,
    /// Block-device write bytes.
    pub block_write_bytes: u64,
    /// Number of processes/threads in the cgroup.
    pub pids: u32,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            available: false,
            cpu_percent: 0.0,
            mem_bytes: 0,
            mem_limit_bytes: 0,
            net_rx_bytes: 0,
            net_tx_bytes: 0,
            block_read_bytes: 0,
            block_write_bytes: 0,
            pids: 0,
        }
    }
}

impl Stats {
    /// Memory used as a 0..=1 fraction of the limit. Returns 0 when the
    /// limit is unknown so the bar reads empty rather than full.
    pub fn mem_fraction(&self) -> f32 {
        if self.mem_limit_bytes == 0 {
            0.0
        } else {
            (self.mem_bytes as f32 / self.mem_limit_bytes as f32).clamp(0.0, 1.0)
        }
    }
}

/// A single container, as projected for the UI.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub status: ContainerStatus,
    /// Free-form status detail, e.g. "Up 4 hours" or "Exited (0) 2 days ago".
    pub status_detail: String,
    pub command: String,
    pub created: String,
    pub ports: Vec<PortMapping>,
    pub mounts: Vec<Mount>,
    pub env: Vec<(String, String)>,
    pub labels: Vec<(String, String)>,
    pub stats: Stats,
    /// Recent log lines (most recent last). Populated lazily for the
    /// selected container.
    pub logs: Vec<String>,
    /// A failed log read is separate from a successful empty log.
    #[serde(default)]
    pub logs_error: Option<String>,
}

impl Container {
    /// 12-character short id, podman-style.
    pub fn short_id(&self) -> &str {
        short_id_of(&self.id)
    }
}

/// A local container image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Image {
    pub id: String,
    pub repository: String,
    pub tag: String,
    pub size_bytes: u64,
    pub created: String,
    /// Number of containers currently using this image.
    pub in_use_by: u32,
}

impl Image {
    pub fn reference(&self) -> String {
        format!("{}:{}", self.repository, self.tag)
    }
}

/// A distrobox — a tightly host-integrated container, listed separately
/// because its lifecycle verbs differ (`enter`, `create`, `stop`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DistroBox {
    pub name: String,
    pub image: String,
    pub status: ContainerStatus,
    pub status_detail: String,
}

/// A named podman volume.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    pub name: String,
    pub driver: String,
    /// Host mountpoint of the volume (`.../volumes/<name>/_data`).
    pub mountpoint: String,
    pub created: String,
    /// Number of containers currently mounting this volume.
    pub in_use_by: u32,
}

/// The complete, in-memory snapshot the UI renders from.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Engine {
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub volumes: Vec<Volume>,
    pub distroboxes: Vec<DistroBox>,
    /// Identity string for the title bar — `podman` version / connection,
    /// or "sample data" when offline.
    pub connection: String,
    /// True when this snapshot came from a live engine rather than the
    /// bundled sample.
    pub live: bool,
}

impl Engine {
    /// Total bytes across all images.
    pub fn image_disk_bytes(&self) -> u64 {
        self.images.iter().map(|i| i.size_bytes).sum()
    }

    pub fn find(&self, id: &str) -> Option<&Container> {
        self.containers.iter().find(|c| c.id == id)
    }
}

// =====================================================================
// Sample data (for --shots and as a graceful offline fallback)
// =====================================================================

/// Which top-level section is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Containers,
    Images,
    Volumes,
    Distrobox,
}

impl Section {
    pub fn as_str(self) -> &'static str {
        match self {
            Section::Containers => "containers",
            Section::Images => "images",
            Section::Volumes => "volumes",
            Section::Distrobox => "distrobox",
        }
    }
    /// The section a key names; `None` for any other text.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "containers" => Some(Section::Containers),
            "images" => Some(Section::Images),
            "volumes" => Some(Section::Volumes),
            "distrobox" => Some(Section::Distrobox),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Section::Containers => "Containers",
            Section::Images => "Images",
            Section::Volumes => "Volumes",
            Section::Distrobox => "Distrobox",
        }
    }
}

/// Which tab of the container detail view is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailTab {
    Logs,
    Stats,
    Inspect,
    Terminal,
}

impl DetailTab {
    pub fn as_str(self) -> &'static str {
        match self {
            DetailTab::Logs => "logs",
            DetailTab::Stats => "stats",
            DetailTab::Inspect => "inspect",
            DetailTab::Terminal => "terminal",
        }
    }
    /// The tab a key names; `None` for any other text.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "logs" => Some(DetailTab::Logs),
            "stats" => Some(DetailTab::Stats),
            "inspect" => Some(DetailTab::Inspect),
            "terminal" => Some(DetailTab::Terminal),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            DetailTab::Logs => "Logs",
            DetailTab::Stats => "Stats",
            DetailTab::Inspect => "Inspect",
            DetailTab::Terminal => "Terminal",
        }
    }
    pub const ALL: [DetailTab; 4] = [
        DetailTab::Logs,
        DetailTab::Stats,
        DetailTab::Inspect,
        DetailTab::Terminal,
    ];
}

/// A populated engine snapshot used when no live podman socket
/// is reachable (notably the headless `--shots` renderer). Mirrors what a
/// developer machine running a small web stack would show.
pub fn sample_engine() -> Engine {
    Engine {
        containers: sample_containers(),
        images: sample_images(),
        volumes: sample_volumes(),
        distroboxes: sample_distroboxes(),
        connection: "Not connected to Podman".into(),
        live: false,
    }
}

fn sample_containers() -> Vec<Container> {
    vec![
        sample_container_web_frontend(),
        sample_container_api_server(),
        sample_container_postgres(),
        sample_container_redis_cache(),
        sample_container_build_runner(),
        sample_container_minio(),
        sample_container_grafana(),
    ]
}

fn sample_container_web_frontend() -> Container {
    Container {
        id: "9f3c1a7be4d2c0118af26b5d4e9f01a2".into(),
        name: "web-frontend".into(),
        image: "docker.io/library/nginx:1.27-alpine".into(),
        status: ContainerStatus::Running,
        status_detail: "Up 4 hours".into(),
        command: "nginx -g 'daemon off;'".into(),
        created: "4 hours ago".into(),
        ports: vec![
            PortMapping {
                host_port: 8080,
                container_port: 80,
                protocol: "tcp".into(),
            },
            PortMapping {
                host_port: 8443,
                container_port: 443,
                protocol: "tcp".into(),
            },
        ],
        mounts: vec![
            Mount {
                source: "/var/workspace/site/dist".into(),
                destination: "/usr/share/nginx/html".into(),
                read_only: true,
            },
            Mount {
                source: "nginx-conf".into(),
                destination: "/etc/nginx/conf.d".into(),
                read_only: false,
            },
        ],
        env: vec![
            ("NGINX_HOST".into(), "localhost".into()),
            ("NGINX_PORT".into(), "80".into()),
            ("TZ".into(), "UTC".into()),
        ],
        labels: vec![
            ("io.tilo.cluster".into(), "dev-stack".into()),
            ("maintainer".into(), "NGINX Docker Maintainers".into()),
        ],
        stats: Stats {
            available: true,
            cpu_percent: 0.07,
            mem_bytes: 18 * 1024 * 1024,
            mem_limit_bytes: 256 * 1024 * 1024,
            net_rx_bytes: 4_812_344,
            net_tx_bytes: 19_004_221,
            block_read_bytes: 1_204_224,
            block_write_bytes: 90_112,
            pids: 5,
        },
        logs_error: None,
        logs: sample_nginx_logs(),
    }
}

fn sample_container_api_server() -> Container {
    Container {
        id: "1b77e0a9c3f54d2e8e0c9a1f6b2d3e4f".into(),
        name: "api-server".into(),
        image: "ghcr.io/northwind/api:2.4.1".into(),
        status: ContainerStatus::Running,
        status_detail: "Up 4 hours (healthy)".into(),
        command: "/usr/local/bin/api --config /etc/api/config.toml".into(),
        created: "4 hours ago".into(),
        ports: vec![PortMapping {
            host_port: 3000,
            container_port: 3000,
            protocol: "tcp".into(),
        }],
        mounts: vec![
            Mount {
                source: "/var/workspace/api/config".into(),
                destination: "/etc/api".into(),
                read_only: true,
            },
            Mount {
                source: "api-data".into(),
                destination: "/var/lib/api".into(),
                read_only: false,
            },
        ],
        env: vec![
            ("RUST_LOG".into(), "info".into()),
            ("DATABASE_URL".into(), "postgres://db:5432/app".into()),
            ("PORT".into(), "3000".into()),
        ],
        labels: vec![("io.tilo.cluster".into(), "dev-stack".into())],
        stats: Stats {
            available: true,
            cpu_percent: 0.23,
            mem_bytes: 142 * 1024 * 1024,
            mem_limit_bytes: 512 * 1024 * 1024,
            net_rx_bytes: 88_120_004,
            net_tx_bytes: 41_993_120,
            block_read_bytes: 7_340_032,
            block_write_bytes: 2_097_152,
            pids: 14,
        },
        logs_error: None,
        logs: sample_api_logs(),
    }
}

fn sample_container_postgres() -> Container {
    Container {
        id: "c4e8d2b0a6f9417c83b1d5e7f0a9c2b4".into(),
        name: "postgres".into(),
        image: "docker.io/library/postgres:16".into(),
        status: ContainerStatus::Running,
        status_detail: "Up 4 hours (healthy)".into(),
        command: "postgres".into(),
        created: "4 hours ago".into(),
        ports: vec![PortMapping {
            host_port: 5432,
            container_port: 5432,
            protocol: "tcp".into(),
        }],
        mounts: vec![Mount {
            source: "pgdata".into(),
            destination: "/var/lib/postgresql/data".into(),
            read_only: false,
        }],
        env: vec![
            ("POSTGRES_DB".into(), "app".into()),
            ("POSTGRES_USER".into(), "app".into()),
            ("PGDATA".into(), "/var/lib/postgresql/data/pgdata".into()),
        ],
        labels: vec![("io.tilo.cluster".into(), "dev-stack".into())],
        stats: Stats {
            available: true,
            cpu_percent: 0.04,
            mem_bytes: 96 * 1024 * 1024,
            mem_limit_bytes: 1024 * 1024 * 1024,
            net_rx_bytes: 12_004_221,
            net_tx_bytes: 33_882_104,
            block_read_bytes: 41_943_040,
            block_write_bytes: 134_217_728,
            pids: 9,
        },
        logs_error: None,
        logs: sample_postgres_logs(),
    }
}

fn sample_container_redis_cache() -> Container {
    Container {
        id: "7a2f9c4e1d8b6053f2a9c1e4b7d0f3a6".into(),
        name: "redis-cache".into(),
        image: "docker.io/library/redis:7-alpine".into(),
        status: ContainerStatus::Running,
        status_detail: "Up 4 hours".into(),
        command: "redis-server --save 60 1".into(),
        created: "4 hours ago".into(),
        ports: vec![PortMapping {
            host_port: 6379,
            container_port: 6379,
            protocol: "tcp".into(),
        }],
        mounts: vec![],
        env: vec![("TZ".into(), "UTC".into())],
        labels: vec![("io.tilo.cluster".into(), "dev-stack".into())],
        stats: Stats {
            available: true,
            cpu_percent: 0.01,
            mem_bytes: 6 * 1024 * 1024,
            mem_limit_bytes: 128 * 1024 * 1024,
            net_rx_bytes: 2_204_004,
            net_tx_bytes: 1_882_104,
            block_read_bytes: 0,
            block_write_bytes: 524_288,
            pids: 4,
        },
        logs_error: None,
        logs: vec![
            "1:C 30 Jun 2026 06:42:02.114 * Redis version=7.2.5, bits=64, pid=1, just started"
                .into(),
            "1:M 30 Jun 2026 06:42:02.116 * Ready to accept connections tcp".into(),
        ],
    }
}

fn sample_container_build_runner() -> Container {
    Container {
        id: "e0b3a7c9f2d14e6b8c0a3f5d7e9b1c2d".into(),
        name: "build-runner".into(),
        image: "ghcr.io/northwind/ci-runner:latest".into(),
        status: ContainerStatus::Exited,
        status_detail: "Exited (0) 2 hours ago".into(),
        command: "/runner/entrypoint.sh".into(),
        created: "yesterday".into(),
        ports: vec![],
        mounts: vec![Mount {
            source: "/run/user/1000/podman/podman.sock".into(),
            destination: "/var/run/docker.sock".into(),
            read_only: false,
        }],
        env: vec![
            ("CI".into(), "true".into()),
            ("RUNNER_LABELS".into(), "tilo,rust".into()),
        ],
        labels: vec![("io.tilo.cluster".into(), "ci".into())],
        stats: Stats::default(),
        logs_error: None,
        logs: vec![
            "[runner] job tilo-build#4821 picked up".into(),
            "[runner] cargo build -p shell ... ok (184.2s)".into(),
            "[runner] job complete, exit 0".into(),
        ],
    }
}

fn sample_container_minio() -> Container {
    Container {
        id: "5d1c8b3a0e7f24698b2d6a0c4e8f1b3d".into(),
        name: "minio".into(),
        image: "docker.io/minio/minio:latest".into(),
        status: ContainerStatus::Exited,
        status_detail: "Exited (137) 3 days ago".into(),
        command: "minio server /data --console-address :9001".into(),
        created: "5 days ago".into(),
        ports: vec![
            PortMapping {
                host_port: 9000,
                container_port: 9000,
                protocol: "tcp".into(),
            },
            PortMapping {
                host_port: 9001,
                container_port: 9001,
                protocol: "tcp".into(),
            },
        ],
        mounts: vec![Mount {
            source: "minio-data".into(),
            destination: "/data".into(),
            read_only: false,
        }],
        env: vec![
            ("MINIO_ROOT_USER".into(), "admin".into()),
            ("MINIO_ROOT_PASSWORD".into(), "********".into()),
        ],
        labels: vec![],
        stats: Stats::default(),
        logs_error: None,
        logs: vec![
            "API: http://0.0.0.0:9000".into(),
            "WARNING: Console endpoint is listening on a dynamic port".into(),
            "Received signal SIGKILL, exiting".into(),
        ],
    }
}

fn sample_container_grafana() -> Container {
    Container {
        id: "a9f0c3e7b1d24856c8a0f2e4d6b8c1a3".into(),
        name: "grafana".into(),
        image: "docker.io/grafana/grafana:11.1.0".into(),
        status: ContainerStatus::Paused,
        status_detail: "Paused".into(),
        command: "/run.sh".into(),
        created: "2 days ago".into(),
        ports: vec![PortMapping {
            host_port: 3001,
            container_port: 3000,
            protocol: "tcp".into(),
        }],
        mounts: vec![Mount {
            source: "grafana-storage".into(),
            destination: "/var/lib/grafana".into(),
            read_only: false,
        }],
        env: vec![("GF_SECURITY_ADMIN_USER".into(), "admin".into())],
        labels: vec![("io.tilo.cluster".into(), "observability".into())],
        stats: Stats {
            available: true,
            cpu_percent: 0.0,
            mem_bytes: 54 * 1024 * 1024,
            mem_limit_bytes: 256 * 1024 * 1024,
            net_rx_bytes: 502_004,
            net_tx_bytes: 188_210,
            block_read_bytes: 0,
            block_write_bytes: 0,
            pids: 11,
        },
        logs_error: None,
        logs: vec![
            "logger=settings t=2026-06-28 level=info msg=\"Starting Grafana\" version=11.1.0"
                .into(),
            "logger=http.server level=info msg=\"HTTP Server Listen\" address=[::]:3000".into(),
        ],
    }
}

fn sample_images() -> Vec<Image> {
    vec![
        Image {
            id: "sha256:5a3c8f9b2e7d14c6a8b0f2e4d6c8a0b2".into(),
            repository: "docker.io/library/nginx".into(),
            tag: "1.27-alpine".into(),
            size_bytes: 48_234_496,
            created: "2 weeks ago".into(),
            in_use_by: 1,
        },
        Image {
            id: "sha256:b7e0d2a4c6f81930e5a7c9b1d3f5a7c9".into(),
            repository: "ghcr.io/northwind/api".into(),
            tag: "2.4.1".into(),
            size_bytes: 122_683_392,
            created: "3 days ago".into(),
            in_use_by: 1,
        },
        Image {
            id: "sha256:c8f1e3b5a7d92041f6b8d0c2e4a6c8e0".into(),
            repository: "docker.io/library/postgres".into(),
            tag: "16".into(),
            size_bytes: 438_304_768,
            created: "1 month ago".into(),
            in_use_by: 1,
        },
        Image {
            id: "sha256:d9a2f4c6b8e03152a7c9e1d3f5b7d9f1".into(),
            repository: "docker.io/library/redis".into(),
            tag: "7-alpine".into(),
            size_bytes: 41_156_608,
            created: "3 weeks ago".into(),
            in_use_by: 1,
        },
        Image {
            id: "sha256:e0b3a5c7d9f14263b8d0f2a4c6e8b0d2".into(),
            repository: "docker.io/minio/minio".into(),
            tag: "latest".into(),
            size_bytes: 156_237_824,
            created: "5 days ago".into(),
            in_use_by: 1,
        },
        Image {
            id: "sha256:f1c4b6d8e0a25374c9e1a3b5d7f9c1e3".into(),
            repository: "ghcr.io/northwind/ci-runner".into(),
            tag: "latest".into(),
            size_bytes: 689_438_720,
            created: "yesterday".into(),
            in_use_by: 1,
        },
        Image {
            id: "sha256:02d5c7e9f1b36485d0a2b4c6e8f0a2c4".into(),
            repository: "docker.io/library/debian".into(),
            tag: "12-slim".into(),
            size_bytes: 74_973_184,
            created: "2 months ago".into(),
            in_use_by: 0,
        },
    ]
}

fn sample_distroboxes() -> Vec<DistroBox> {
    vec![
        DistroBox {
            name: "my-distrobox".into(),
            image: "registry.fedoraproject.org/fedora-toolbox:42".into(),
            status: ContainerStatus::Running,
            status_detail: "Up 6 hours".into(),
        },
        DistroBox {
            name: "ubuntu-dev".into(),
            image: "docker.io/library/ubuntu:24.04".into(),
            status: ContainerStatus::Exited,
            status_detail: "Exited 1 day ago".into(),
        },
        DistroBox {
            name: "arch-aur".into(),
            image: "docker.io/library/archlinux:latest".into(),
            status: ContainerStatus::Exited,
            status_detail: "Created".into(),
        },
    ]
}

fn sample_volumes() -> Vec<Volume> {
    vec![
        Volume {
            name: "pgdata".into(),
            driver: "local".into(),
            mountpoint: "/var/home/dev/.local/share/containers/storage/volumes/pgdata/_data".into(),
            created: "5 days ago".into(),
            in_use_by: 1,
        },
        Volume {
            name: "api-data".into(),
            driver: "local".into(),
            mountpoint: "/var/home/dev/.local/share/containers/storage/volumes/api-data/_data"
                .into(),
            created: "5 days ago".into(),
            in_use_by: 1,
        },
        Volume {
            name: "grafana-storage".into(),
            driver: "local".into(),
            mountpoint:
                "/var/home/dev/.local/share/containers/storage/volumes/grafana-storage/_data".into(),
            created: "2 days ago".into(),
            in_use_by: 1,
        },
        Volume {
            name: "minio-data".into(),
            driver: "local".into(),
            mountpoint: "/var/home/dev/.local/share/containers/storage/volumes/minio-data/_data"
                .into(),
            created: "5 days ago".into(),
            in_use_by: 0,
        },
        Volume {
            name: "nginx-conf".into(),
            driver: "local".into(),
            mountpoint: "/var/home/dev/.local/share/containers/storage/volumes/nginx-conf/_data"
                .into(),
            created: "4 hours ago".into(),
            in_use_by: 1,
        },
    ]
}

fn sample_nginx_logs() -> Vec<String> {
    vec![
        "/docker-entrypoint.sh: Configuration complete; ready for start up".into(),
        "2026/06/30 06:42:01 [notice] 1#1: using the \"epoll\" event method".into(),
        "2026/06/30 06:42:01 [notice] 1#1: nginx/1.27.0".into(),
        "2026/06/30 06:42:01 [notice] 1#1: OS: Linux 7.0.9".into(),
        "2026/06/30 06:42:01 [notice] 1#1: start worker processes".into(),
        "10.0.2.14 - - [30/Jun/2026:10:32:11 +0000] \"GET / HTTP/1.1\" 200 1024".into(),
        "10.0.2.14 - - [30/Jun/2026:10:32:11 +0000] \"GET /app.css HTTP/1.1\" 200 8210".into(),
        "10.0.2.14 - - [30/Jun/2026:10:32:12 +0000] \"GET /app.js HTTP/1.1\" 200 41200".into(),
        "10.0.2.31 - - [30/Jun/2026:10:35:44 +0000] \"GET /favicon.ico HTTP/1.1\" 404 153".into(),
        "10.0.2.14 - - [30/Jun/2026:10:39:02 +0000] \"GET /api/health HTTP/1.1\" 200 17".into(),
    ]
}

fn sample_api_logs() -> Vec<String> {
    vec![
        "2026-06-30T06:42:02Z INFO  api: starting api-server v2.4.1".into(),
        "2026-06-30T06:42:02Z INFO  api::db: connecting to postgres://db:5432/app".into(),
        "2026-06-30T06:42:02Z INFO  api::db: pool ready (max=16)".into(),
        "2026-06-30T06:42:02Z INFO  api: listening on 0.0.0.0:3000".into(),
        "2026-06-30T10:32:12Z INFO  api::http: GET /api/health 200 0.4ms".into(),
        "2026-06-30T10:35:01Z INFO  api::http: POST /api/login 200 38ms".into(),
        "2026-06-30T10:35:01Z WARN  api::auth: 1 failed login for user=guest".into(),
        "2026-06-30T10:39:02Z INFO  api::http: GET /api/items 200 12ms".into(),
    ]
}

fn sample_postgres_logs() -> Vec<String> {
    vec![
        "PostgreSQL Database directory appears to contain a database; Skipping initialization"
            .into(),
        "2026-06-30 06:42:02.001 UTC [1] LOG:  starting PostgreSQL 16.3".into(),
        "2026-06-30 06:42:02.004 UTC [1] LOG:  listening on IPv4 address \"0.0.0.0\", port 5432"
            .into(),
        "2026-06-30 06:42:02.010 UTC [1] LOG:  database system is ready to accept connections"
            .into(),
        "2026-06-30 10:35:01.221 UTC [42] LOG:  checkpoint starting: time".into(),
        "2026-06-30 10:35:11.882 UTC [42] LOG:  checkpoint complete: wrote 12 buffers".into(),
    ]
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;

    /// Status strings normalise, memory renders in IEC units and memory fractions clamp.
    #[test]
    fn status_and_memory_values_normalise() {
        // --- phase: status_parse_normalises_common_strings ---
        {
            assert_eq!(
                ContainerStatus::parse("Up 4 hours"),
                ContainerStatus::Running
            );
            assert_eq!(ContainerStatus::parse("running"), ContainerStatus::Running);
            assert_eq!(
                ContainerStatus::parse("Exited (0) 2 days ago"),
                ContainerStatus::Exited
            );
            assert_eq!(ContainerStatus::parse("created"), ContainerStatus::Created);
            assert_eq!(ContainerStatus::parse("paused"), ContainerStatus::Paused);
            assert_eq!(ContainerStatus::parse("garbage"), ContainerStatus::Dead);
        }

        // --- phase: mem_bytes_render_with_iec_binary_units ---
        {
            // Container memory is base-1024, so it must wear IEC binary units.
            assert_eq!(ui::format::memory(512), "512 B");
            assert_eq!(ui::format::memory(2048), "2.0 KiB");
            assert_eq!(ui::format::memory(5 * 1024 * 1024), "5.0 MiB");
            assert_eq!(ui::format::memory(3 * 1024 * 1024 * 1024), "3.0 GiB");
        }

        // --- phase: mem_fraction_is_clamped ---
        {
            let mut s = Stats {
                mem_bytes: 200,
                mem_limit_bytes: 100,
                ..Stats::default()
            };
            assert_eq!(s.mem_fraction(), 1.0);
            s.mem_limit_bytes = 0;
            assert_eq!(s.mem_fraction(), 0.0);
        }
    }

    /// The sample engine is populated and consistent, with named volumes.
    #[test]
    fn sample_engine_is_consistent() {
        // --- phase: sample_engine_is_populated_and_consistent ---
        {
            let e = sample_engine();
            let running = e
                .containers
                .iter()
                .filter(|c| c.status.is_running())
                .count();
            assert!(running >= 3, "sample has several running containers");
            assert!(
                e.containers.len() - running >= 2,
                "sample has stopped containers too"
            );
            assert!(!e.images.is_empty());
            assert!(!e.distroboxes.is_empty());
            // Each running container should carry some logs for the detail view.
            for c in e.containers.iter().filter(|c| c.status.is_running()) {
                assert!(!c.logs.is_empty(), "{} has no sample logs", c.name);
            }
        }

        // --- phase: sample_engine_has_volumes ---
        {
            let e = sample_engine();
            assert!(!e.volumes.is_empty(), "sample carries named volumes");
            assert!(e.volumes.iter().any(|v| v.name == "pgdata"));
        }
    }
}
