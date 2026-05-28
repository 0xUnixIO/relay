use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};

mod auth;
mod backup;
mod cache;
mod config;
mod db;
mod enroll;
mod grpc;
mod health_checker;
mod http;
mod maintenance;
mod models;
mod pause;
mod pki;
mod ports;
mod registry;
mod scheduler;
mod series;
mod snowflake;
mod state;
mod stats_flush;
mod upgrade;

#[derive(Debug, Parser)]
#[command(name = "relay-master", version, about = "Relay control plane")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Run the control plane (default if no subcommand is given).
    Serve,
    /// Database operations (init / migrate).
    Db(DbArgs),
}

#[derive(Debug, clap::Args)]
struct DbArgs {
    #[command(subcommand)]
    cmd: DbCmd,
}

#[derive(Debug, Subcommand)]
enum DbCmd {
    /// Bootstrap the relay role + database. Connects with a Postgres
    /// superuser URL (e.g. postgres://postgres:pw@localhost/postgres) and
    /// CREATE ROLE / CREATE DATABASE if missing. Idempotent.
    Init {
        /// Superuser DSN, e.g. postgres://postgres:pw@localhost/postgres
        #[arg(long, env = "MASTER_ADMIN_DATABASE_URL")]
        admin_url: String,
        /// Role to create (default: relay).
        #[arg(long, default_value = "relay")]
        role: String,
        /// Password for the new role.
        #[arg(long, env = "MASTER_DB_PASSWORD")]
        password: String,
        /// Database to create (default: relay).
        #[arg(long, default_value = "relay")]
        database: String,
    },
    /// Apply embedded SQL migrations against MASTER_DATABASE_URL.
    /// Migrations also run automatically on `serve` startup; this is for
    /// running them manually (e.g. before flipping a service).
    Migrate,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    relay_common::logging::init("relay-master");

    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Serve) {
        Cmd::Serve => serve().await,
        Cmd::Db(DbArgs { cmd }) => match cmd {
            DbCmd::Init {
                admin_url,
                role,
                password,
                database,
            } => db_init(&admin_url, &role, &password, &database).await,
            DbCmd::Migrate => db_migrate().await,
        },
    }
}

async fn serve() -> Result<()> {
    let cfg = config::Config::from_env()?;

    let pki = std::sync::Arc::new(pki::Pki::ensure(&cfg.pki_dir, &cfg.public_addrs)?);
    tracing::info!(
        pki_dir = %cfg.pki_dir.display(),
        sans = ?cfg.public_addrs,
        "PKI ready"
    );

    let pool = db::connect(&cfg.database_url).await?;

    // 可选 Redis：用于 probe 防抖等轻量缓存。连不上不阻挡启动，运行期内 ConnectionManager 会自行重连。
    let redis = if let Some(url) = cfg.redis_url.clone() {
        match redis::Client::open(url.clone()) {
            Ok(client) => match redis::aio::ConnectionManager::new(client).await {
                Ok(mgr) => {
                    tracing::info!("redis ready (cache layer enabled)");
                    Some(mgr)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to connect to MASTER_REDIS_URL; continuing without cache");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "MASTER_REDIS_URL invalid; continuing without cache");
                None
            }
        }
    } else {
        tracing::info!("MASTER_REDIS_URL not set; running without redis cache");
        None
    };

    let http_addr: std::net::SocketAddr = cfg.http_addr.parse()?;
    let grpc_addr: std::net::SocketAddr = cfg.grpc_addr.parse()?;
    tracing::info!(%http_addr, %grpc_addr, "starting relay-master");

    if cfg.enroll_public_url.is_none() {
        let http_is_local =
            cfg.http_addr.starts_with("127.0.0.1") || cfg.http_addr.starts_with("[::1]");
        if http_is_local {
            tracing::warn!(
                http_addr = %cfg.http_addr,
                "MASTER_ENROLL_PUBLIC_URL is not set and HTTP listener is bound to loopback — \
                 节点 enroll 拿到的 fallback URL 将不可达。请在 .env 中显式设置 \
                 MASTER_ENROLL_PUBLIC_URL（如反代域名 https://relay.example.com/api/v1/enroll）"
            );
        }
    }

    let state = state::AppState::new(cfg, pool, pki.clone(), redis);

    // 启动时一次性从 Redis warmup 心跳 L1，让重启后立即拥有运行时视图。
    if state.redis.is_some() {
        let warm = cache::node::warmup(&state.redis).await;
        if !warm.is_empty() {
            let now = chrono::Utc::now();
            let mut map = state.node_runtime.write().await;
            for (id, p) in warm {
                map.insert(
                    id,
                    state::NodeRuntimeEntry {
                        last_heartbeat: p.last_heartbeat,
                        last_seen_at: p.last_seen_at,
                        version: p.version,
                        protocol_version: p.protocol_version,
                        capabilities: p.capabilities,
                        // 视为"刚刚已写"，避免 warmup 后第一条心跳又触发 PG 写
                        last_pg_write_at: now,
                    },
                );
            }
            tracing::info!(count = map.len(), "node heartbeat L1 warmed from redis");
        }
    }

    scheduler::spawn(state.db.clone(), state.registry.clone());
    maintenance::spawn(state.db.clone());
    backup::spawn(state.db.clone(), state.backup_trigger.clone());
    stats_flush::spawn(state.clone());

    let http_task = tokio::spawn(http::serve(http_addr, state.clone()));
    let grpc_task = tokio::spawn(grpc::serve(grpc_addr, state.clone(), pki.clone()));

    tokio::select! {
        r = http_task => r??,
        r = grpc_task => r??,
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutdown signal received");
        }
    }

    Ok(())
}

/// Idempotently create the relay role + database using a superuser DSN.
/// Identifiers are validated against `[a-z0-9_]+` since pg_quote_ident is
/// not exposed to clients and we don't want to hand-roll quoting.
async fn db_init(admin_url: &str, role: &str, password: &str, database: &str) -> Result<()> {
    use sqlx::Connection;
    if !is_safe_ident(role) || !is_safe_ident(database) {
        return Err(anyhow!(
            "role and database must start with [a-z_] and contain only [a-z0-9_], length 1-63 (got role={role:?}, database={database:?})"
        ));
    }

    let mut conn = sqlx::postgres::PgConnection::connect(admin_url)
        .await
        .with_context(|| "connecting with admin DSN")?;

    // Build DDL on the server using format(%I, %L) so Postgres handles
    // identifier and string-literal quoting for us — no hand-escaping.
    let role_exists: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM pg_roles WHERE rolname = $1")
        .bind(role)
        .fetch_optional(&mut conn)
        .await?;
    if role_exists.is_none() {
        let sql: (String,) =
            sqlx::query_as("SELECT format('CREATE ROLE %I LOGIN PASSWORD %L', $1::text, $2::text)")
                .bind(role)
                .bind(password)
                .fetch_one(&mut conn)
                .await?;
        sqlx::query(&sql.0).execute(&mut conn).await?;
        tracing::info!(role, "created role");
    } else {
        let sql: (String,) = sqlx::query_as(
            "SELECT format('ALTER ROLE %I WITH LOGIN PASSWORD %L', $1::text, $2::text)",
        )
        .bind(role)
        .bind(password)
        .fetch_one(&mut conn)
        .await?;
        sqlx::query(&sql.0).execute(&mut conn).await?;
        tracing::info!(role, "role exists, password updated");
    }

    let db_exists: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM pg_database WHERE datname = $1")
        .bind(database)
        .fetch_optional(&mut conn)
        .await?;
    if db_exists.is_none() {
        let sql: (String,) =
            sqlx::query_as("SELECT format('CREATE DATABASE %I OWNER %I', $1::text, $2::text)")
                .bind(database)
                .bind(role)
                .fetch_one(&mut conn)
                .await?;
        sqlx::query(&sql.0).execute(&mut conn).await?;
        tracing::info!(database, owner = role, "created database");
    } else {
        tracing::info!(database, "database exists, skipping create");
    }

    println!(
        "✓ ready. Set MASTER_DATABASE_URL=postgres://{role}:<password>@<host>/{database} and run `relay-master`"
    );
    Ok(())
}

async fn db_migrate() -> Result<()> {
    let url = std::env::var("MASTER_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .map_err(|_| anyhow!("MASTER_DATABASE_URL is required"))?;
    let _pool = db::connect(&url).await?;
    println!("✓ migrations up to date");
    Ok(())
}

fn is_safe_ident(s: &str) -> bool {
    if s.is_empty() || s.len() > 63 {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_lowercase() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}
