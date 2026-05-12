use std::collections::HashSet;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::StreamExt as TokioStreamExt;
use tower_http::cors::{Any, CorsLayer};

use crate::auth::{self, Claims};
use crate::cache;
use crate::models::{
    Forward, ForwardPort, ForwardView, GroupMemberView, GroupTunnel, GroupTunnelView, Node, Tunnel,
    TunnelHopRef, TunnelView, User, UserGroup, UserGroupView, UserTunnel, UserTunnelView,
};
use crate::state::AppState;

#[derive(rust_embed::Embed)]
#[folder = "../../web/dist/"]
struct WebAssets;

pub async fn serve(addr: SocketAddr, state: AppState) -> anyhow::Result<()> {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION]);

    let admin = Router::new()
        .route("/api/v1/nodes", get(list_nodes).post(create_node))
        .route(
            "/api/v1/nodes/:id",
            get(get_node).put(update_node).delete(delete_node),
        )
        .route("/api/v1/nodes/speeds", get(get_all_node_speeds))
        .route("/api/v1/nodes/speeds/stream", get(node_speeds_stream))
        .route("/api/v1/nodes/:id/series", get(get_node_series))
        .route("/api/v1/nodes/:id/rotate-token", post(rotate_node_token))
        .route("/api/v1/nodes/:id/probe-port", post(probe_node_port))
        .route("/api/v1/tunnels", post(create_tunnel))
        .route(
            "/api/v1/tunnels/:id",
            get(get_tunnel).put(update_tunnel).delete(delete_tunnel),
        )
        .route("/api/v1/tunnels/:id/probe", post(probe_tunnel))
        .route("/api/v1/tunnels/:id/probe-stream", get(probe_tunnel_stream))
        .route(
            "/api/v1/user-tunnels",
            get(list_user_tunnels).post(create_user_tunnel),
        )
        .route(
            "/api/v1/user-tunnels/:id",
            axum::routing::put(update_user_tunnel).delete(delete_user_tunnel),
        )
        .route("/api/v1/users", get(list_users).post(create_user))
        .route(
            "/api/v1/users/:id",
            axum::routing::put(update_user).delete(delete_user),
        )
        .route("/api/v1/config", axum::routing::put(update_config))
        .route(
            "/api/v1/user-groups",
            get(list_user_groups).post(create_user_group),
        )
        .route(
            "/api/v1/user-groups/:id",
            get(get_user_group)
                .put(update_user_group)
                .delete(delete_user_group),
        )
        .route(
            "/api/v1/user-groups/:id/members",
            get(list_group_members).post(add_group_member),
        )
        .route(
            "/api/v1/user-groups/:id/members/:user_id",
            axum::routing::delete(remove_group_member),
        )
        .route(
            "/api/v1/user-groups/:id/tunnels",
            get(list_group_tunnels).post(create_group_tunnel),
        )
        .route(
            "/api/v1/user-groups/:id/tunnels/:gt_id",
            axum::routing::put(update_group_tunnel).delete(delete_group_tunnel),
        )
        .route("/api/v1/user-groups/:id/apply", post(apply_group_tunnels))
        .route("/api/v1/system/version", get(get_system_version))
        .route("/api/v1/system/self-upgrade", post(upgrade_self_handler))
        .route(
            "/api/v1/system/upgrade_channel",
            get(get_upgrade_channel).put(put_upgrade_channel),
        )
        .route("/api/v1/nodes/:id/setup", post(create_setup))
        .route("/api/v1/nodes/:id/upgrade", post(create_node_upgrade))
        .route(
            "/api/v1/nodes/:id/upgrade/jobs",
            get(list_node_upgrade_jobs),
        )
        .route("/api/v1/upgrade_jobs/:id", get(get_upgrade_job))
        .route("/api/v1/system/branding", axum::routing::put(put_branding))
        .route(
            "/api/v1/system/backup/r2",
            get(get_r2_backup_config).put(put_r2_backup_config),
        )
        .route("/api/v1/system/backup/trigger", post(trigger_backup))
        .route("/api/v1/system/backup/jobs", get(list_backup_jobs))
        .route("/api/v1/system/backup/files", get(list_backup_files))
        .route(
            "/api/v1/system/backup/restore",
            post(restore_backup_handler),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_admin_layer,
        ));

    let user = Router::new()
        .route("/api/v1/config", get(get_config))
        .route("/api/v1/server-info", get(server_info))
        .route("/api/v1/tunnels", get(list_tunnels))
        .route("/api/v1/users/me/tunnels", get(list_my_user_tunnels))
        .route("/api/v1/forwards", get(list_forwards).post(create_forward))
        .route(
            "/api/v1/forwards/:id",
            get(get_forward).put(update_forward).delete(delete_forward),
        )
        .route("/api/v1/forwards/:id/pause", post(pause_forward))
        .route("/api/v1/forwards/:id/resume", post(resume_forward))
        .route("/api/v1/forwards/:id/redeploy", post(redeploy_forward))
        .route("/api/v1/forwards/:id/probe", post(probe_forward))
        .route(
            "/api/v1/forwards/:id/probe-stream",
            get(probe_forward_stream),
        )
        .route("/api/v1/forwards/batch/delete", post(batch_delete_forwards))
        .route("/api/v1/forwards/batch/pause", post(batch_pause_forwards))
        .route("/api/v1/forwards/batch/resume", post(batch_resume_forwards))
        .route(
            "/api/v1/forwards/batch/redeploy",
            post(batch_redeploy_forwards),
        )
        .route("/api/v1/auth/me", get(get_me))
        .route("/api/v1/auth/me/password", post(change_own_password))
        .route("/api/v1/events/forwards", get(forward_stats_stream))
        .route("/api/v1/forwards/:id/stats", get(forward_stats_history))
        .route(
            "/api/v1/forwards/:id/connections",
            get(forward_connections_handler),
        )
        .route("/api/v1/forwards/:id/probe-stats", get(forward_probe_stats))
        .route(
            "/api/v1/forwards/:id/probe-series",
            get(forward_probe_series),
        )
        .route("/api/v1/stats/traffic", get(global_traffic_stats_handler))
        .route(
            "/api/v1/stats/nodes/traffic",
            get(node_traffic_stats_handler),
        );

    let protected = admin
        .merge(user)
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    let public = Router::new()
        .route(
            "/health",
            get(|| async {
                (
                    StatusCode::OK,
                    [(axum::http::header::CACHE_CONTROL, "no-store")],
                    "ok",
                )
            }),
        )
        .route("/api/v1/status", get(public_status))
        .route("/api/v1/status/stream", get(public_status_stream))
        .route("/api/v1/auth/status", get(auth_status))
        .route("/api/v1/auth/bootstrap", post(bootstrap_admin))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/enroll", post(crate::enroll::enroll_handler))
        .route("/api/v1/setup/:token", get(get_setup))
        .route("/api/v1/system/branding", get(get_branding))
        .route("/scripts/:name", get(proxy_script));

    let app = public
        .merge(protected)
        .fallback(static_handler)
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") || path == "health" {
        return (StatusCode::NOT_FOUND, "资源不存在").into_response();
    }
    if let Some(resp) = serve_embedded(path) {
        return resp;
    }
    if let Some(resp) = serve_embedded("index.html") {
        return resp;
    }
    (
        StatusCode::NOT_FOUND,
        "web bundle not embedded — build it with `bun run build` in /web before `cargo build`",
    )
        .into_response()
}

fn serve_embedded(path: &str) -> Option<Response> {
    let path = if path.is_empty() { "index.html" } else { path };
    let file = WebAssets::get(path)?;
    let mime = file.metadata.mimetype();
    Some(
        Response::builder()
            .header(header::CONTENT_TYPE, mime)
            .body(Body::from(file.data.into_owned()))
            .unwrap(),
    )
}

async fn require_auth(
    State(s): State<AppState>,
    mut req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Result<Response, ApiError> {
    // 优先取 Authorization header，SSE 场景下 fallback 到 ?token= query param
    let token: String = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned)
        .or_else(|| {
            req.uri().query().and_then(|q| {
                q.split('&')
                    .find_map(|p| p.strip_prefix("token=").map(str::to_owned))
            })
        })
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "缺少 bearer 令牌"))?;
    let claims = auth::verify_jwt(&s.cfg.jwt_secret, &token)
        .map_err(|_| ApiError::new(StatusCode::UNAUTHORIZED, "令牌无效"))?;
    req.extensions_mut().insert(claims);
    Ok(next.run(req).await)
}

fn require_admin(claims: &Claims) -> Result<(), ApiError> {
    if claims.role != "admin" {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "仅管理员"));
    }
    Ok(())
}

async fn require_admin_layer(
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Result<Response, ApiError> {
    let claims = req
        .extensions()
        .get::<Claims>()
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "未认证"))?;
    require_admin(claims)?;
    Ok(next.run(req).await)
}

// ---------- Errors ----------

pub struct ApiError {
    status: StatusCode,
    msg: String,
    payload: Option<serde_json::Value>,
}
impl ApiError {
    pub(crate) fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        Self {
            status,
            msg: msg.into(),
            payload: None,
        }
    }
}
impl<E: std::fmt::Display> From<E> for ApiError {
    fn from(e: E) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({"error": self.msg});
        if let Some(extra) = self.payload {
            if let (Some(obj), Some(extras)) = (body.as_object_mut(), extra.as_object()) {
                for (k, v) in extras {
                    obj.insert(k.clone(), v.clone());
                }
            }
        }
        (self.status, Json(body)).into_response()
    }
}
pub type ApiResult<T> = Result<T, ApiError>;

fn random_token() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

const PORT_PROBE_MAX_RETRIES: u32 = 3;

/// 逐跳探测已分配的端口是否真的可以 bind。
/// - 节点离线/超时 → 加入 warnings，跳过。
/// - bind 失败 → 调用 reallocate_one_port 换端口，最多重试 PORT_PROBE_MAX_RETRIES 次。
/// - 重试耗尽仍失败 → 返回 Err（调用方应删除 forward 并回传错误）。
/// - 入口跳（hop_index=0）端口发生变化时，同步更新 forwards.in_port。
async fn probe_and_fix_ports(
    s: &AppState,
    forward_id: i64,
    protocols: &[String],
) -> Result<Vec<String>, ApiError> {
    use crate::registry::ProbeError;
    use relay_proto::v1::ProbeKind;

    // Layered DAG: 同一 hop_index 下多节点共享 listen_port（由 DB 触发器保证）。
    // 按 hop_index 聚合后，对每个节点都做一次 bind 探测；任一节点任一协议失败
    // → 整层重分配新公共端口，再以新端口对全层重新探测。
    let rows: Vec<(i32, String, i32)> = sqlx::query_as(
        "SELECT DISTINCT hop_index, node_id, listen_port
           FROM forward_ports WHERE forward_id = $1 ORDER BY hop_index, node_id",
    )
    .bind(forward_id)
    .fetch_all(&s.db)
    .await?;

    use std::collections::BTreeMap;
    let mut layers: BTreeMap<i32, (i32, Vec<String>)> = BTreeMap::new();
    for (hop_index, node_id, listen_port) in rows {
        let entry = layers.entry(hop_index).or_insert((listen_port, Vec::new()));
        entry.1.push(node_id);
    }

    let proto_slice: Vec<&str> = protocols.iter().map(|s| s.as_str()).collect();
    let mut warnings: Vec<String> = Vec::new();

    for (hop_index, (initial_port, node_ids)) in layers {
        let mut port = initial_port;

        'retry: for attempt in 0..=PORT_PROBE_MAX_RETRIES {
            let mut layer_ok = true;
            let mut last_error = String::new();
            let mut failed_node: Option<String> = None;

            'nodes: for node_id in &node_ids {
                for proto in protocols {
                    let kind = if proto == "udp" {
                        ProbeKind::BindUdp
                    } else {
                        ProbeKind::BindTcp
                    };
                    let target = format!("[::]:{port}");
                    match s
                        .registry
                        .probe_with_kind(node_id, target, std::time::Duration::from_secs(2), kind)
                        .await
                    {
                        Ok(result) if result.ok => continue,
                        Ok(result) => {
                            layer_ok = false;
                            last_error = format!(
                                "节点 {node_id} {} 协议探测失败：{}",
                                proto.to_uppercase(),
                                result.error
                            );
                            failed_node = Some(node_id.clone());
                            break 'nodes;
                        }
                        Err(ProbeError::NodeOffline | ProbeError::Timeout) => {
                            warnings.push(format!("节点 {node_id} 未连接，跳过端口探测"));
                            // 单节点离线不阻塞整层；继续探测同层其他节点。
                            break;
                        }
                    }
                }
            }

            if layer_ok {
                break 'retry;
            }
            if attempt == PORT_PROBE_MAX_RETRIES {
                let who = failed_node.unwrap_or_default();
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    format!(
                        "第 {} 层端口 {port} 被占用，{attempt} 次重分配后仍失败（{who}）：{last_error}",
                        hop_index + 1
                    ),
                ));
            }
            let new_port = crate::ports::reallocate_layer_port(
                &s.db,
                forward_id,
                hop_index,
                &proto_slice,
                None,
            )
            .await?;
            tracing::info!(
                hop_index,
                old_port = port,
                new_port,
                attempt,
                "端口被占用，已重新分配整层"
            );
            port = new_port;
        }
    }

    Ok(warnings)
}

fn caller_id(claims: &Claims) -> Result<i64, ApiError> {
    claims
        .sub
        .parse()
        .map_err(|_| ApiError::new(StatusCode::UNAUTHORIZED, "令牌无效"))
}

// ---------- Auth ----------

#[derive(Serialize)]
pub struct AuthStatus {
    pub bootstrapped: bool,
}

async fn auth_status(State(s): State<AppState>) -> ApiResult<Json<AuthStatus>> {
    let count: (i64,) = sqlx::query_as("SELECT count(*) FROM users")
        .fetch_one(&s.db)
        .await?;
    Ok(Json(AuthStatus {
        bootstrapped: count.0 > 0,
    }))
}

#[derive(Deserialize)]
pub struct BootstrapReq {
    pub username: String,
    pub password: String,
}

async fn bootstrap_admin(
    State(s): State<AppState>,
    Json(req): Json<BootstrapReq>,
) -> ApiResult<Json<User>> {
    let count: (i64,) = sqlx::query_as("SELECT count(*) FROM users")
        .fetch_one(&s.db)
        .await?;
    if count.0 > 0 {
        return Err(ApiError::new(StatusCode::CONFLICT, "已存在初始化用户"));
    }
    let hash = auth::hash_password(&req.password)?;
    let row: User = sqlx::query_as(
        "INSERT INTO users (id, username, password_hash, role) VALUES ($1, $2, $3, 'admin') RETURNING *",
    )
    .bind(crate::snowflake::next_id())
    .bind(&req.username)
    .bind(&hash)
    .fetch_one(&s.db)
    .await?;
    Ok(Json(row))
}

#[derive(Deserialize)]
pub struct LoginReq {
    pub username: String,
    pub password: String,
}
#[derive(Serialize)]
pub struct LoginResp {
    pub token: String,
    pub username: String,
    pub role: String,
}

async fn login(State(s): State<AppState>, Json(req): Json<LoginReq>) -> ApiResult<Json<LoginResp>> {
    let user: Option<User> = sqlx::query_as("SELECT * FROM users WHERE username = $1")
        .bind(&req.username)
        .fetch_optional(&s.db)
        .await?;
    let user = user.ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "用户名或密码错误"))?;
    if !auth::verify_password(&req.password, &user.password_hash)? {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "用户名或密码错误"));
    }
    if user.status == "disabled" {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "账号已被禁用"));
    }
    if let Some(exp) = user.expires_at {
        if exp <= chrono::Utc::now() {
            return Err(ApiError::new(StatusCode::FORBIDDEN, "账号已过期"));
        }
    }
    if user.status == "expired" {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "账号已过期"));
    }
    let token = auth::issue_jwt(&s.cfg.jwt_secret, &user.id.to_string(), &user.role)?;
    Ok(Json(LoginResp {
        token,
        username: user.username,
        role: user.role,
    }))
}

// ---------- Users (admin) ----------

const VALID_ROLES: [&str; 2] = ["admin", "user"];
const VALID_STATUSES: [&str; 3] = ["active", "disabled", "expired"];

fn validate_role(role: &str) -> Result<(), ApiError> {
    if !VALID_ROLES.contains(&role) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "角色必须是 admin/user",
        ));
    }
    Ok(())
}

fn validate_status(s: &str) -> Result<(), ApiError> {
    if !VALID_STATUSES.contains(&s) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "状态必须是 active/disabled/expired",
        ));
    }
    Ok(())
}

fn validate_password(password: &str) -> Result<(), ApiError> {
    if password.len() < 6 {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "密码长度至少 6 位",
        ));
    }
    Ok(())
}

fn map_db_err(e: sqlx::Error) -> ApiError {
    if let sqlx::Error::Database(db_err) = &e {
        if db_err.code().as_deref() == Some("23505") {
            return ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "唯一约束冲突");
        }
    }
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

async fn admin_count(db: &sqlx::PgPool) -> Result<i64, ApiError> {
    let row: (i64,) = sqlx::query_as("SELECT count(*) FROM users WHERE role = 'admin'")
        .fetch_one(db)
        .await?;
    Ok(row.0)
}

#[derive(Serialize, sqlx::FromRow)]
pub struct UserListItem {
    #[sqlx(flatten)]
    #[serde(flatten)]
    pub user: User,
    pub group_name: Option<String>,
}

async fn list_users(State(s): State<AppState>) -> ApiResult<Json<Vec<UserListItem>>> {
    let rows: Vec<UserListItem> = sqlx::query_as(
        "SELECT u.*,
                (SELECT g.name FROM group_members gm
                   JOIN user_groups g ON g.id = gm.group_id
                  WHERE gm.user_id = u.id
                  LIMIT 1) AS group_name
           FROM users u
          ORDER BY u.created_at",
    )
    .fetch_all(&s.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct CreateUserReq {
    pub username: String,
    pub password: String,
    pub role: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub remark: Option<String>,
}

async fn create_user(
    State(s): State<AppState>,
    Json(req): Json<CreateUserReq>,
) -> ApiResult<Json<User>> {
    if req.username.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "用户名不能为空",
        ));
    }
    validate_password(&req.password)?;
    validate_role(&req.role)?;
    if let Some(st) = req.status.as_deref() {
        validate_status(st)?;
    }
    let hash = auth::hash_password(&req.password)?;
    let row: User = sqlx::query_as(
        "INSERT INTO users (id, username, password_hash, role, status, expires_at, remark)
         VALUES ($1, $2, $3, $4, COALESCE($5,'active'), $6, COALESCE($7,''))
         RETURNING *",
    )
    .bind(crate::snowflake::next_id())
    .bind(&req.username)
    .bind(&hash)
    .bind(&req.role)
    .bind(req.status.as_deref())
    .bind(req.expires_at)
    .bind(req.remark.as_deref())
    .fetch_one(&s.db)
    .await
    .map_err(map_db_err)?;
    Ok(Json(row))
}

#[derive(Deserialize)]
pub struct UpdateUserReq {
    pub role: Option<String>,
    pub password: Option<String>,
    pub status: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub expires_at: Option<Option<DateTime<Utc>>>,
    pub remark: Option<String>,
}

fn deserialize_optional_field<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

async fn update_user(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateUserReq>,
) -> ApiResult<Json<User>> {
    let nothing = req.role.is_none()
        && req.password.is_none()
        && req.status.is_none()
        && req.expires_at.is_none()
        && req.remark.is_none();
    if nothing {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "至少需要提供一个可修改字段",
        ));
    }
    let target: Option<User> = sqlx::query_as("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(&s.db)
        .await?;
    let target = target.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "用户不存在"))?;

    if let Some(new_role) = req.role.as_deref() {
        validate_role(new_role)?;
        if target.role == "admin" && new_role != "admin" && admin_count(&s.db).await? <= 1 {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "至少要保留一个 admin",
            ));
        }
    }
    if let Some(st) = req.status.as_deref() {
        validate_status(st)?;
    }
    let new_hash = if let Some(pw) = req.password.as_deref() {
        validate_password(pw)?;
        Some(auth::hash_password(pw)?)
    } else {
        None
    };

    let row: User = sqlx::query_as(
        "UPDATE users SET
            role          = COALESCE($2, role),
            password_hash = COALESCE($3, password_hash),
            status        = COALESCE($4, status),
            expires_at    = CASE WHEN $5::bool THEN $6 ELSE expires_at END,
            remark        = COALESCE($7, remark),
            updated_at    = now()
         WHERE id = $1 RETURNING *",
    )
    .bind(id)
    .bind(req.role.as_deref())
    .bind(new_hash.as_deref())
    .bind(req.status.as_deref())
    .bind(req.expires_at.is_some())
    .bind(req.expires_at.flatten())
    .bind(req.remark.as_deref())
    .fetch_one(&s.db)
    .await
    .map_err(map_db_err)?;
    if req.status.is_some() || req.expires_at.is_some() {
        crate::scheduler::kick(s.db.clone(), s.registry.clone());
    }
    Ok(Json(row))
}

async fn delete_user(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    if claims.sub == id.to_string() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "不能删除当前登录账号",
        ));
    }
    let target: Option<User> = sqlx::query_as("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(&s.db)
        .await?;
    let target = target.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "用户不存在"))?;
    if target.role == "admin" && admin_count(&s.db).await? <= 1 {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "至少要保留一个 admin",
        ));
    }
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id)
        .execute(&s.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct ChangeOwnPasswordReq {
    pub old_password: String,
    pub new_password: String,
}

async fn change_own_password(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ChangeOwnPasswordReq>,
) -> ApiResult<StatusCode> {
    validate_password(&req.new_password)?;
    let id = caller_id(&claims)?;
    let user: Option<User> = sqlx::query_as("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(&s.db)
        .await?;
    let user = user.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "用户不存在"))?;
    if !auth::verify_password(&req.old_password, &user.password_hash)? {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "原密码错误"));
    }
    let hash = auth::hash_password(&req.new_password)?;
    sqlx::query("UPDATE users SET password_hash = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(&hash)
        .execute(&s.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct MeResp {
    #[serde(flatten)]
    pub user: User,
    pub forward_count: i64,
    pub user_tunnel_count: i64,
    pub group_name: Option<String>,
    pub flow_limit_bytes: i64,
    pub speed_limit_kbps: i64,
    pub forward_limit: i32,
}

async fn get_me(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<MeResp>> {
    let id = caller_id(&claims)?;
    let user: Option<User> = sqlx::query_as("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(&s.db)
        .await?;
    let user = user.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "用户不存在"))?;
    let forward_count: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM forwards f
           JOIN user_tunnels ut ON ut.id = f.user_tunnel_id
          WHERE ut.user_id = $1",
    )
    .bind(id)
    .fetch_one(&s.db)
    .await?;
    let user_tunnel_count: (i64,) =
        sqlx::query_as("SELECT count(*) FROM user_tunnels WHERE user_id = $1")
            .bind(id)
            .fetch_one(&s.db)
            .await?;
    let group_row: Option<(String, i64, i64, i32)> = sqlx::query_as(
        "SELECT g.name, g.flow_limit_bytes, g.speed_limit_kbps, g.forward_limit
           FROM group_members gm
           JOIN user_groups g ON g.id = gm.group_id
          WHERE gm.user_id = $1
          LIMIT 1",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await?;
    let (group_name, flow_limit_bytes, speed_limit_kbps, forward_limit) =
        group_row.map_or((None, 0i64, 0i64, 0i32), |(n, f, s, t)| (Some(n), f, s, t));
    Ok(Json(MeResp {
        user,
        forward_count: forward_count.0,
        user_tunnel_count: user_tunnel_count.0,
        group_name,
        flow_limit_bytes,
        speed_limit_kbps,
        forward_limit,
    }))
}

// ---------- Nodes ----------

#[derive(Deserialize)]
pub struct CreateNodeReq {
    pub id: Option<String>,
    pub hostname: Option<String>,
    pub tags: Option<Vec<String>>,
    pub port_range_start: Option<i32>,
    pub port_range_end: Option<i32>,
    pub tunnel_eligible: Option<bool>,
}

fn validate_port_range(start: i32, end: i32) -> Result<(), ApiError> {
    if !(1..=65535).contains(&start) || !(1..=65535).contains(&end) || start > end {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "端口范围无效：要求 1 ≤ 起始 ≤ 结束 ≤ 65535",
        ));
    }
    Ok(())
}

#[derive(Serialize)]
pub struct CreateNodeResp {
    #[serde(flatten)]
    pub node: Node,
    pub enrollment_token: String,
}

async fn list_nodes(State(s): State<AppState>) -> ApiResult<Json<Vec<Node>>> {
    let mut rows: Vec<Node> = sqlx::query_as("SELECT * FROM nodes ORDER BY id")
        .fetch_all(&s.db)
        .await?;
    s.overlay_nodes(&mut rows).await;
    Ok(Json(rows))
}

async fn create_node(
    State(s): State<AppState>,
    Json(req): Json<CreateNodeReq>,
) -> ApiResult<Json<CreateNodeResp>> {
    let node_id = match req.id {
        Some(ref id) if !id.trim().is_empty() => {
            if !crate::pki::is_valid_node_id(id) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "节点 ID 须匹配正则 ^[a-z0-9][a-z0-9._-]{0,62}$",
                ));
            }
            id.clone()
        }
        _ => crate::snowflake::next_id().to_string(),
    };
    let port_range_start = req.port_range_start.unwrap_or(30_000);
    let port_range_end = req.port_range_end.unwrap_or(39_999);
    validate_port_range(port_range_start, port_range_end)?;
    let token = random_token();
    let row: Node = sqlx::query_as(
        "INSERT INTO nodes (id, hostname, version, tags, enrollment_token,
                            port_range_start, port_range_end, tunnel_eligible)
         VALUES ($1, $2, '', $3, $4, $5, $6, $7) RETURNING *",
    )
    .bind(&node_id)
    .bind(req.hostname.unwrap_or_default())
    .bind(req.tags.unwrap_or_default())
    .bind(&token)
    .bind(port_range_start)
    .bind(port_range_end)
    .bind(req.tunnel_eligible.unwrap_or(true))
    .fetch_one(&s.db)
    .await?;
    Ok(Json(CreateNodeResp {
        node: row,
        enrollment_token: token,
    }))
}

async fn get_node(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Node>> {
    let row: Option<Node> = sqlx::query_as("SELECT * FROM nodes WHERE id = $1")
        .bind(&id)
        .fetch_optional(&s.db)
        .await?;
    let mut row = row.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "资源不存在"))?;
    s.overlay_node(&mut row).await;
    Ok(Json(row))
}

#[derive(Deserialize)]
pub struct UpdateNodeReq {
    pub hostname: Option<String>,
    pub tags: Option<Vec<String>>,
    pub server_ips: Option<Vec<String>>,
    pub port_range_start: Option<i32>,
    pub port_range_end: Option<i32>,
    pub traffic_ratio: Option<f64>,
    pub tunnel_eligible: Option<bool>,
    pub expires_at: Option<Option<DateTime<chrono::Utc>>>,
    pub monthly_price: Option<Option<f64>>,
    pub website: Option<String>,
}

async fn update_node(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateNodeReq>,
) -> ApiResult<Json<Node>> {
    let server_ips = req.server_ips.map(|v| {
        v.into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
    });
    let prev: Option<(Vec<String>, i32, i32)> = sqlx::query_as(
        "SELECT server_ips, port_range_start, port_range_end FROM nodes WHERE id = $1",
    )
    .bind(&id)
    .fetch_optional(&s.db)
    .await?;
    let (prev_ips, prev_pr_start, prev_pr_end) =
        prev.unwrap_or_else(|| (Vec::new(), 30_000, 39_999));

    if req.port_range_start.is_some() || req.port_range_end.is_some() {
        let new_start = req.port_range_start.unwrap_or(prev_pr_start);
        let new_end = req.port_range_end.unwrap_or(prev_pr_end);
        validate_port_range(new_start, new_end)?;
    }

    if let Some(r) = req.traffic_ratio {
        if r < 0.0 {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "traffic_ratio 不能为负数",
            ));
        }
    }

    let row: Option<Node> = sqlx::query_as(
        "UPDATE nodes SET
            hostname         = COALESCE($2, hostname),
            tags             = COALESCE($3, tags),
            server_ips       = CASE WHEN $4::BOOLEAN THEN $5 ELSE server_ips END,
            port_range_start = COALESCE($6, port_range_start),
            port_range_end   = COALESCE($7, port_range_end),
            traffic_ratio    = COALESCE($8, traffic_ratio),
            tunnel_eligible  = COALESCE($9, tunnel_eligible),
            expires_at       = CASE WHEN $10::BOOLEAN THEN $11 ELSE expires_at END,
            monthly_price    = CASE WHEN $12::BOOLEAN THEN $13 ELSE monthly_price END,
            website          = COALESCE($14, website),
            updated_at       = now()
          WHERE id = $1 RETURNING *",
    )
    .bind(&id)
    .bind(req.hostname)
    .bind(req.tags)
    .bind(server_ips.is_some())
    .bind(server_ips.clone().unwrap_or_default())
    .bind(req.port_range_start)
    .bind(req.port_range_end)
    .bind(req.traffic_ratio)
    .bind(req.tunnel_eligible)
    .bind(req.expires_at.is_some())
    .bind(req.expires_at.flatten())
    .bind(req.monthly_price.is_some())
    .bind(req.monthly_price.flatten())
    .bind(req.website)
    .fetch_optional(&s.db)
    .await?;
    let row = row.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "资源不存在"))?;
    let mut row = row;
    s.overlay_node(&mut row).await;

    let user_provided = server_ips.is_some();
    let new_ips = server_ips.unwrap_or_default();
    if user_provided && new_ips != prev_ips {
        match crate::ports::predecessors_of(&s.db, &id).await {
            Ok(preds) => {
                for pred in &preds {
                    s.registry.push_config(&s.db, pred).await;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, node = %id,
                    "failed to enumerate predecessors after server_ips change");
            }
        }
    }

    Ok(Json(row))
}

async fn delete_node(State(s): State<AppState>, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let res = sqlx::query("DELETE FROM nodes WHERE id = $1")
        .bind(&id)
        .execute(&s.db)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.code().as_deref() == Some("23503") => ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "节点仍被隧道或转发引用，请先解除引用",
            ),
            _ => ApiError::from(e),
        })?;
    if res.rows_affected() == 0 {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "资源不存在"));
    }
    s.registry.force_kick(&id).await;
    // 清理 L1 / L2 心跳运行时缓存，避免重建同名节点继承旧数据。
    s.node_runtime.write().await.remove(&id);
    crate::cache::node::delete(&s.redis, &id).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct RotateTokenResp {
    pub id: String,
    pub enrollment_token: String,
}

async fn rotate_node_token(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<RotateTokenResp>> {
    let token = random_token();
    let row: Option<(String,)> = sqlx::query_as(
        "UPDATE nodes
            SET enrollment_token = $2,
                updated_at       = now()
          WHERE id = $1 RETURNING id",
    )
    .bind(&id)
    .bind(&token)
    .fetch_optional(&s.db)
    .await?;
    if row.is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "资源不存在"));
    }
    Ok(Json(RotateTokenResp {
        id,
        enrollment_token: token,
    }))
}

#[derive(Serialize)]
pub struct ServerInfo {
    pub public_host: String,
    pub public_hosts: Vec<String>,
    pub grpc_port: u16,
    pub master_endpoint: String,
    pub enroll_endpoint: String,
    pub ca_cert_pem: String,
    pub ca_cert_b64: String,
    pub version: String,
}

async fn server_info(State(s): State<AppState>) -> ApiResult<Json<ServerInfo>> {
    use base64::Engine;
    let public_hosts = s.cfg.public_addrs.clone();
    let public_host = public_hosts
        .first()
        .cloned()
        .unwrap_or_else(|| "localhost".into());
    let grpc_port = port_of(&s.cfg.grpc_addr).unwrap_or(7443);
    // 节点 enroll endpoint 选择优先级：
    //   1. 部署侧显式 MASTER_ENROLL_PUBLIC_URL（最准确，反代场景必填）
    //   2. fallback：http://<public_host>:<http_port>/api/v1/enroll
    //      —— 假设 7080 直接对外开（无反代）
    let http_port = port_of(&s.cfg.http_addr).unwrap_or(7080);
    let enroll_endpoint = s
        .cfg
        .enroll_public_url
        .clone()
        .unwrap_or_else(|| format!("http://{public_host}:{http_port}/api/v1/enroll"));
    let ca_pem = s.pki.ca_cert_pem.clone();
    let ca_b64 = base64::engine::general_purpose::STANDARD.encode(ca_pem.as_bytes());
    Ok(Json(ServerInfo {
        master_endpoint: format!("https://{public_host}:{grpc_port}"),
        enroll_endpoint,
        public_host,
        public_hosts,
        grpc_port,
        ca_cert_pem: ca_pem,
        ca_cert_b64: ca_b64,
        version: env!("CARGO_PKG_VERSION").to_string(),
    }))
}

fn port_of(addr: &str) -> Option<u16> {
    addr.rsplit_once(':')?.1.parse().ok()
}

async fn create_setup(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    use base64::Engine;
    let row: Option<(String,)> = sqlx::query_as("SELECT enrollment_token FROM nodes WHERE id = $1")
        .bind(&id)
        .fetch_optional(&s.db)
        .await?;
    let (enrollment_token,) =
        row.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "资源不存在"))?;

    let public_host = s
        .cfg
        .public_addrs
        .first()
        .cloned()
        .unwrap_or_else(|| "localhost".into());
    let grpc_port = port_of(&s.cfg.grpc_addr).unwrap_or(7443);
    let http_port = port_of(&s.cfg.http_addr).unwrap_or(7080);
    let enroll_endpoint = s
        .cfg
        .enroll_public_url
        .clone()
        .unwrap_or_else(|| format!("http://{public_host}:{http_port}/api/v1/enroll"));
    let ca_b64 = base64::engine::general_purpose::STANDARD.encode(s.pki.ca_cert_pem.as_bytes());

    let token = {
        use rand::RngCore;
        let mut buf = [0u8; 8];
        rand::thread_rng().fill_bytes(&mut buf);
        hex::encode(buf)
    };

    s.setup_tokens.write().await.insert(
        token.clone(),
        crate::state::SetupEntry {
            node_id: id,
            enrollment_token,
            master_endpoint: format!("https://{public_host}:{grpc_port}"),
            enroll_endpoint,
            ca_cert_b64: ca_b64,
            created_at: std::time::Instant::now(),
        },
    );

    Ok(Json(json!({ "token": token })))
}

#[derive(Serialize)]
struct SetupPayload {
    master: String,
    enroll: String,
    node_id: String,
    token: String,
    ca_cert: String,
}

async fn get_setup(
    State(s): State<AppState>,
    Path(token): Path<String>,
) -> ApiResult<Json<SetupPayload>> {
    let bundles = s.setup_tokens.read().await;
    let entry = bundles
        .get(&token)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "安装包不存在或已过期"))?;
    if entry.created_at.elapsed() > std::time::Duration::from_secs(86400) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "安装包已过期"));
    }
    Ok(Json(SetupPayload {
        master: entry.master_endpoint.clone(),
        enroll: entry.enroll_endpoint.clone(),
        node_id: entry.node_id.clone(),
        token: entry.enrollment_token.clone(),
        ca_cert: entry.ca_cert_b64.clone(),
    }))
}

async fn get_all_node_speeds(
    State(s): State<AppState>,
) -> Json<std::collections::HashMap<String, crate::state::NodeSpeed>> {
    Json(s.speed_tx.borrow().clone())
}

async fn node_speeds_stream(
    State(s): State<AppState>,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    use tokio_stream::wrappers::WatchStream;

    let rx = s.speed_tx.subscribe();
    let stream = WatchStream::new(rx).map(|snapshot| {
        Ok::<_, Infallible>(match serde_json::to_string(&snapshot) {
            Ok(j) => Event::default().data(j),
            Err(_) => Event::default().comment("serialize error"),
        })
    });

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

async fn get_node_series(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<crate::series::NodeSeries>> {
    #[derive(sqlx::FromRow)]
    struct HbRow {
        ts_ms: i64,
        cpu_pct: f64,
        mem_used_bytes: i64,
        mem_total_bytes: i64,
        active_connections: i32,
    }

    let rows: Vec<HbRow> = sqlx::query_as(
        "SELECT EXTRACT(EPOCH FROM ts)::BIGINT * 1000 AS ts_ms,
                cpu_pct, mem_used_bytes, mem_total_bytes, active_connections
           FROM node_heartbeats
          WHERE node_id = $1
          ORDER BY ts DESC
          LIMIT 120",
    )
    .bind(&id)
    .fetch_all(&s.db)
    .await
    .unwrap_or_default();

    let mut heartbeats: Vec<crate::series::HeartbeatSample> = rows
        .into_iter()
        .map(|r| crate::series::HeartbeatSample {
            ts_unix_ms: r.ts_ms,
            cpu_pct: r.cpu_pct,
            mem_used_bytes: r.mem_used_bytes as u64,
            mem_total_bytes: r.mem_total_bytes as u64,
            active_connections: r.active_connections as u32,
        })
        .collect();
    heartbeats.reverse(); // DESC 查出来倒序，翻转为时间正序

    let tunnels = s.series.tunnel_series(&id).await;

    Ok(Json(crate::series::NodeSeries {
        heartbeats,
        tunnels,
    }))
}

#[derive(Deserialize)]
pub struct ProbePortReq {
    pub port: i32,
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

fn default_protocol() -> String {
    "tcp".to_string()
}

#[derive(Serialize)]
struct ProbePortResponse {
    free: bool,
    error: String,
}

async fn probe_node_port(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ProbePortReq>,
) -> ApiResult<Json<ProbePortResponse>> {
    if !(1..=65535).contains(&req.port) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "端口需在 1-65535 之间",
        ));
    }
    let kind = match req.protocol.to_lowercase().as_str() {
        "tcp" => relay_proto::v1::ProbeKind::BindTcp,
        "udp" => relay_proto::v1::ProbeKind::BindUdp,
        _ => {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "protocol 必须是 tcp 或 udp",
            ));
        }
    };
    let target = format!("[::]:{}", req.port);
    match s
        .registry
        .probe_with_kind(&id, target, std::time::Duration::from_secs(2), kind)
        .await
    {
        Ok(res) => Ok(Json(ProbePortResponse {
            free: res.ok,
            error: res.error,
        })),
        Err(crate::registry::ProbeError::NodeOffline) => {
            Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "节点未连接"))
        }
        Err(crate::registry::ProbeError::Timeout) => {
            Err(ApiError::new(StatusCode::GATEWAY_TIMEOUT, "探测超时"))
        }
    }
}

// ---------- Tunnels ----------

const VALID_PROTOCOLS: [&str; 2] = ["tcp", "udp"];
const VALID_IP_PREF: [&str; 3] = ["", "ipv4", "ipv6"];
const VALID_LB: [&str; 5] = [
    "round_robin",
    "random",
    "least_latency",
    "primary_backup",
    "best",
];

/// 校验并规范化 tunnel 协议集合：lowercase + dedupe + sort，必须是 ["tcp","udp"] 的非空子集。
fn validate_protocols(input: &[String]) -> Result<Vec<String>, ApiError> {
    if input.is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "至少需要一个协议",
        ));
    }
    let mut out: Vec<String> = input.iter().map(|s| s.trim().to_lowercase()).collect();
    out.sort();
    out.dedup();
    for p in &out {
        if !VALID_PROTOCOLS.contains(&p.as_str()) {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "protocols 仅支持 tcp 或 udp",
            ));
        }
    }
    if out.len() > VALID_PROTOCOLS.len() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "protocols 数量超过支持的范围",
        ));
    }
    Ok(out)
}

fn validate_ip_pref(p: &str) -> Result<(), ApiError> {
    if !VALID_IP_PREF.contains(&p) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "ip_preference 必须是空字符串/ipv4/ipv6",
        ));
    }
    Ok(())
}

fn validate_lb(p: &str) -> Result<(), ApiError> {
    if !VALID_LB.contains(&p) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "lb_strategy 必须是 round_robin / primary_backup / best 之一",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct TunnelReq {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_tunnel_protocols")]
    pub protocols: Vec<String>,
    #[serde(default)]
    pub ip_preference: String,
    #[serde(default)]
    pub in_ip: String,
    /// Legacy linear path. Mutually exclusive with `layers`.
    #[serde(default)]
    pub node_ids: Vec<String>,
    /// Layered DAG: `layers[i]` is the set of node IDs at hop_index = i.
    /// Each layer ≥ 1 node; a node may appear in at most one layer (self-
    /// loop banned by DB UNIQUE (tunnel_id, node_id)).
    #[serde(default)]
    pub layers: Option<Vec<Vec<String>>>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_tunnel_protocols() -> Vec<String> {
    vec!["tcp".to_string(), "udp".to_string()]
}

fn default_true() -> bool {
    true
}

async fn list_tunnels(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<Vec<TunnelView>>> {
    let tunnels: Vec<Tunnel> = if claims.role == "admin" {
        sqlx::query_as("SELECT * FROM tunnels ORDER BY name")
            .fetch_all(&s.db)
            .await?
    } else {
        // 非管理员只能看到自己套餐内已分配且 enabled 的隧道
        let user_id = caller_id(&claims)?;
        sqlx::query_as(
            "SELECT t.* FROM tunnels t
               JOIN user_tunnels ut ON ut.tunnel_id = t.id
              WHERE ut.user_id = $1 AND ut.enabled = true
              ORDER BY t.name",
        )
        .bind(user_id)
        .fetch_all(&s.db)
        .await?
    };
    let mut out = Vec::with_capacity(tunnels.len());
    for t in tunnels {
        out.push(load_tunnel_view(&s.db, t).await?);
    }
    Ok(Json(out))
}

async fn load_tunnel_view(db: &sqlx::PgPool, t: Tunnel) -> Result<TunnelView, ApiError> {
    let hop_rows: Vec<(i32, String)> = sqlx::query_as(
        "SELECT hop_index, node_id FROM tunnel_hops WHERE tunnel_id = $1
          ORDER BY hop_index, node_id",
    )
    .bind(t.id)
    .fetch_all(db)
    .await?;
    let count: (i64,) = sqlx::query_as("SELECT count(*) FROM user_tunnels WHERE tunnel_id = $1")
        .bind(t.id)
        .fetch_one(db)
        .await?;
    let fwd_count: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM forwards f
           JOIN user_tunnels ut ON ut.id = f.user_tunnel_id
          WHERE ut.tunnel_id = $1",
    )
    .bind(t.id)
    .fetch_one(db)
    .await?;
    // 查询该隧道所有节点的 hostname，用于前端展示
    let name_rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT n.id::text, n.hostname FROM nodes n
           JOIN tunnel_hops th ON th.node_id = n.id
          WHERE th.tunnel_id = $1",
    )
    .bind(t.id)
    .fetch_all(db)
    .await?;
    let node_names: std::collections::HashMap<String, String> = name_rows.into_iter().collect();
    // Group flat rows into layers: layers[hop_index] = [node_id, ...]
    let mut layer_map: std::collections::BTreeMap<i32, Vec<String>> =
        std::collections::BTreeMap::new();
    for (i, n) in &hop_rows {
        layer_map.entry(*i).or_default().push(n.clone());
    }
    let layers: Vec<Vec<String>> = layer_map.into_values().collect();
    let is_layered = layers.iter().any(|l| l.len() > 1);
    Ok(TunnelView {
        tunnel: t,
        hops: hop_rows
            .into_iter()
            .map(|(i, n)| TunnelHopRef {
                hop_index: i,
                node_id: n,
            })
            .collect(),
        layers,
        is_layered,
        user_tunnel_count: count.0,
        forward_count: fwd_count.0,
        node_names,
    })
}

/// Validate a layered DAG path. Each layer must be non-empty; nodes must
/// be unique across the whole path; non-entry layer nodes must have at
/// least one server_ip; all referenced nodes must be tunnel_eligible.
async fn validate_tunnel_layers(db: &sqlx::PgPool, layers: &[Vec<String>]) -> Result<(), ApiError> {
    const MAX_NODES_PER_LAYER: usize = 8;
    if layers.is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "至少需要一层节点",
        ));
    }
    let mut seen: HashSet<String> = HashSet::new();
    for (i, layer) in layers.iter().enumerate() {
        if layer.is_empty() {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("第 {} 层为空", i + 1),
            ));
        }
        if layer.len() > MAX_NODES_PER_LAYER {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("第 {} 层节点数超过上限 {MAX_NODES_PER_LAYER}", i + 1),
            ));
        }
        for n in layer {
            if n.trim().is_empty() {
                return Err(ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "节点 ID 不能为空",
                ));
            }
            if !seen.insert(n.clone()) {
                return Err(ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("节点 {n} 在路径中重复出现"),
                ));
            }
        }
    }
    let all_nodes: Vec<String> = layers.iter().flatten().cloned().collect();
    let rows: Vec<(String, Vec<String>, bool)> =
        sqlx::query_as("SELECT id, server_ips, tunnel_eligible FROM nodes WHERE id = ANY($1)")
            .bind(&all_nodes)
            .fetch_all(db)
            .await?;
    let by_id: std::collections::HashMap<_, _> = rows
        .into_iter()
        .map(|(id, ips, eligible)| (id, (ips, eligible)))
        .collect();
    for (idx, layer) in layers.iter().enumerate() {
        for n in layer {
            let (ips, eligible) = by_id.get(n).ok_or_else(|| {
                ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, format!("节点不存在：{n}"))
            })?;
            if !eligible {
                return Err(ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("节点 {n} 未开放隧道用途（tunnel_eligible = false）"),
                ));
            }
            if idx > 0 && !ips.iter().any(|s| !s.trim().is_empty()) {
                return Err(ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("节点 {n} 作为中转/出口前需先配置 server_ips"),
                ));
            }
        }
    }
    Ok(())
}

/// Resolve a TunnelReq's path into the canonical layered form. Either
/// `layers` (preferred) or `node_ids` (legacy linear) must be provided.
fn resolve_layers_from_req(
    layers: &Option<Vec<Vec<String>>>,
    node_ids: &[String],
) -> Result<Vec<Vec<String>>, ApiError> {
    match (layers, node_ids.is_empty()) {
        (Some(l), true) => Ok(l.clone()),
        (Some(_), false) => Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "layers 与 node_ids 不能同时提供",
        )),
        (None, false) => Ok(node_ids.iter().map(|n| vec![n.clone()]).collect()),
        (None, true) => Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "至少需要一个节点",
        )),
    }
}

async fn create_tunnel(
    State(s): State<AppState>,
    Json(req): Json<TunnelReq>,
) -> ApiResult<Json<TunnelView>> {
    if req.name.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "name 不能为空",
        ));
    }
    let protocols = validate_protocols(&req.protocols)?;
    validate_ip_pref(&req.ip_preference)?;
    let layers = resolve_layers_from_req(&req.layers, &req.node_ids)?;
    validate_tunnel_layers(&s.db, &layers).await?;

    let mut tx = s.db.begin().await?;
    let t: Tunnel = sqlx::query_as(
        "INSERT INTO tunnels (id, name, description, protocols, ip_preference, in_ip, enabled)
         VALUES ($1,$2,$3,$4,$5,$6,$7)
         RETURNING *",
    )
    .bind(crate::snowflake::next_id())
    .bind(&req.name)
    .bind(&req.description)
    .bind(&protocols)
    .bind(&req.ip_preference)
    .bind(&req.in_ip)
    .bind(req.enabled)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_err)?;

    for (i, layer) in layers.iter().enumerate() {
        for n in layer {
            sqlx::query(
                "INSERT INTO tunnel_hops (tunnel_id, hop_index, node_id) VALUES ($1, $2, $3)",
            )
            .bind(t.id)
            .bind(i as i32)
            .bind(n)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(Json(load_tunnel_view(&s.db, t).await?))
}

async fn get_tunnel(State(s): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<TunnelView>> {
    let t: Option<Tunnel> = sqlx::query_as("SELECT * FROM tunnels WHERE id = $1")
        .bind(id)
        .fetch_optional(&s.db)
        .await?;
    let t = t.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "隧道不存在"))?;
    Ok(Json(load_tunnel_view(&s.db, t).await?))
}

#[derive(Deserialize)]
pub struct UpdateTunnelReq {
    pub name: Option<String>,
    pub description: Option<String>,
    pub protocols: Option<Vec<String>>,
    pub ip_preference: Option<String>,
    pub in_ip: Option<String>,
    pub enabled: Option<bool>,
    pub node_ids: Option<Vec<String>>,
    pub layers: Option<Vec<Vec<String>>>,
}

async fn update_tunnel(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateTunnelReq>,
) -> ApiResult<Json<TunnelView>> {
    let normalized_protocols = match &req.protocols {
        Some(ps) => Some(validate_protocols(ps)?),
        None => None,
    };
    if let Some(p) = req.ip_preference.as_deref() {
        validate_ip_pref(p)?;
    }
    if req.node_ids.is_some() && req.layers.is_some() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "layers 与 node_ids 不能同时提供",
        ));
    }
    let path_change = req.node_ids.is_some() || req.layers.is_some();

    // Resolve incoming path to layered form, with 409 protect: when the
    // existing tunnel is layered (any hop has >1 nodes) and the caller
    // passes the legacy linear `node_ids`, we refuse to flatten silently.
    let new_layers: Option<Vec<Vec<String>>> = if path_change {
        let existing: Vec<(i32, String)> =
            sqlx::query_as("SELECT hop_index, node_id FROM tunnel_hops WHERE tunnel_id = $1")
                .bind(id)
                .fetch_all(&s.db)
                .await?;
        let mut map: std::collections::BTreeMap<i32, usize> = std::collections::BTreeMap::new();
        for (i, _) in &existing {
            *map.entry(*i).or_default() += 1;
        }
        let existing_is_layered = map.values().any(|&c| c > 1);
        if existing_is_layered && req.node_ids.is_some() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "现有隧道为分层结构，请使用 layers 字段更新路径",
            ));
        }
        let resolved = if let Some(ls) = req.layers.clone() {
            ls
        } else {
            req.node_ids
                .as_ref()
                .unwrap()
                .iter()
                .map(|n| vec![n.clone()])
                .collect()
        };
        validate_tunnel_layers(&s.db, &resolved).await?;
        Some(resolved)
    } else {
        None
    };

    if path_change {
        let in_use: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM forwards f
               JOIN user_tunnels ut ON ut.id = f.user_tunnel_id
              WHERE ut.tunnel_id = $1",
        )
        .bind(id)
        .fetch_one(&s.db)
        .await?;
        if in_use.0 > 0 {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "已有转发使用此隧道，无法修改路径；请先删除相关转发",
            ));
        }
    }

    if let Some(new_protocols) = &normalized_protocols {
        let current: (Vec<String>,) = sqlx::query_as("SELECT protocols FROM tunnels WHERE id = $1")
            .bind(id)
            .fetch_optional(&s.db)
            .await?
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "隧道不存在"))?;
        if current.0 != *new_protocols {
            let in_use: (i64,) = sqlx::query_as(
                "SELECT count(*) FROM forwards f
                   JOIN user_tunnels ut ON ut.id = f.user_tunnel_id
                  WHERE ut.tunnel_id = $1",
            )
            .bind(id)
            .fetch_one(&s.db)
            .await?;
            if in_use.0 > 0 {
                return Err(ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "已有转发使用此隧道，无法修改协议；请先删除相关转发",
                ));
            }
        }
    }

    let mut tx = s.db.begin().await?;
    let t: Tunnel = sqlx::query_as(
        "UPDATE tunnels SET
            name          = COALESCE($2, name),
            description   = COALESCE($3, description),
            protocols     = COALESCE($4, protocols),
            ip_preference = COALESCE($5, ip_preference),
            in_ip         = COALESCE($6, in_ip),
            enabled       = COALESCE($7, enabled),
            version       = version + 1,
            updated_at    = now()
          WHERE id = $1 RETURNING *",
    )
    .bind(id)
    .bind(req.name.as_deref())
    .bind(req.description.as_deref())
    .bind(normalized_protocols.as_deref())
    .bind(req.ip_preference.as_deref())
    .bind(req.in_ip.as_deref())
    .bind(req.enabled)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_err)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "隧道不存在"))?;

    if path_change {
        sqlx::query("DELETE FROM tunnel_hops WHERE tunnel_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        for (i, layer) in new_layers.as_ref().unwrap().iter().enumerate() {
            for n in layer {
                sqlx::query(
                    "INSERT INTO tunnel_hops (tunnel_id, hop_index, node_id) VALUES ($1, $2, $3)",
                )
                .bind(id)
                .bind(i as i32)
                .bind(n)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    tx.commit().await?;

    // If enabled flipped, push affected nodes.
    if req.enabled.is_some() {
        let nodes = nodes_for_tunnel(&s.db, id).await?;
        for n in &nodes {
            s.registry.push_config(&s.db, n).await;
        }
        crate::scheduler::kick(s.db.clone(), s.registry.clone());
    }

    Ok(Json(load_tunnel_view(&s.db, t).await?))
}

async fn delete_tunnel(State(s): State<AppState>, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let in_use: (i64,) = sqlx::query_as("SELECT count(*) FROM user_tunnels WHERE tunnel_id = $1")
        .bind(id)
        .fetch_one(&s.db)
        .await?;
    if in_use.0 > 0 {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "已有用户绑定此隧道，无法删除",
        ));
    }
    let res = sqlx::query("DELETE FROM tunnels WHERE id = $1")
        .bind(id)
        .execute(&s.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "隧道不存在"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, Deserialize)]
struct TunnelProbeSegment {
    from_node: String,
    to: String,
    ok: bool,
    latency_us: u64,
    error: String,
}

#[derive(Serialize, Deserialize)]
struct TunnelProbeResponse {
    segments: Vec<TunnelProbeSegment>,
}

fn tunnel_probe_seg(
    from_node: String,
    to: String,
    result: Result<relay_proto::v1::ProbeResult, crate::registry::ProbeError>,
) -> TunnelProbeSegment {
    match result {
        Ok(res) => TunnelProbeSegment {
            from_node,
            to,
            ok: res.ok,
            latency_us: res.latency_us,
            error: res.error,
        },
        Err(crate::registry::ProbeError::NodeOffline) => TunnelProbeSegment {
            from_node,
            to,
            ok: false,
            latency_us: 0,
            error: "节点未连接".into(),
        },
        Err(crate::registry::ProbeError::Timeout) => TunnelProbeSegment {
            from_node,
            to,
            ok: false,
            latency_us: 0,
            error: "探测超时".into(),
        },
    }
}

async fn probe_tunnel(
    State(s): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<TunnelProbeResponse>> {
    // probe 走 admin-only 路由（require_admin_layer），鉴权已在 layer 完成；
    // 缓存 key 仅按 tunnel id 即可。命中直接返回 5s 内的旧结果做点击防抖。
    // v2 因为分层 DAG 段形态变化（同层 fan-out）必须升级 key。
    let cache_key = format!("probe:tunnel:v2:{}", id);
    if let Some(cached) = cache::get_json::<TunnelProbeResponse>(&s.redis, &cache_key).await {
        return Ok(Json(cached));
    }

    let rows: Vec<(i32, String, Vec<String>, i32, i32)> = sqlx::query_as(
        "SELECT th.hop_index, th.node_id, n.server_ips,
                n.port_range_start, n.port_range_end
           FROM tunnel_hops th
           JOIN nodes n ON n.id = th.node_id
          WHERE th.tunnel_id = $1
          ORDER BY th.hop_index, th.node_id",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await?;

    if rows.is_empty() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "隧道不存在或无节点"));
    }

    // Group rows into layers by hop_index.
    struct LayerNode {
        node_id: String,
        ips: Vec<String>,
        range_start: i32,
        range_end: i32,
    }
    let mut layer_map: std::collections::BTreeMap<i32, Vec<LayerNode>> =
        std::collections::BTreeMap::new();
    for (hop, node_id, ips, rs, re) in rows {
        layer_map.entry(hop).or_default().push(LayerNode {
            node_id,
            ips,
            range_start: rs,
            range_end: re,
        });
    }
    let layers: Vec<Vec<LayerNode>> = layer_map.into_values().collect();

    let probe_timeout = Duration::from_secs(1);
    let mut segments: Vec<TunnelProbeSegment> = Vec::new();

    // 跨层段：layers[i] 的每个节点 × layers[i+1] 的每个节点
    for i in 0..layers.len().saturating_sub(1) {
        for from in &layers[i] {
            for next in &layers[i + 1] {
                let next_ip = match next.ips.iter().find(|s| !s.trim().is_empty()) {
                    Some(ip) => ip.clone(),
                    None => {
                        segments.push(TunnelProbeSegment {
                            from_node: from.node_id.clone(),
                            to: next.node_id.clone(),
                            ok: false,
                            latency_us: 0,
                            error: "节点无可用 IP".into(),
                        });
                        continue;
                    }
                };

                let probe_port = crate::ports::pick_free_port(
                    &s.db,
                    &next.node_id,
                    next.range_start + 1,
                    next.range_end,
                    &[],
                )
                .await?;
                let probe_port = match probe_port {
                    Some(p) => p,
                    None => {
                        segments.push(TunnelProbeSegment {
                            from_node: from.node_id.clone(),
                            to: next.node_id.clone(),
                            ok: false,
                            latency_us: 0,
                            error: "节点无可用探测端口".into(),
                        });
                        continue;
                    }
                };

                let bind_hold = probe_timeout + Duration::from_secs(2);
                let bind = s
                    .registry
                    .probe_with_kind(
                        &next.node_id,
                        format!("[::]:{probe_port}"),
                        bind_hold,
                        relay_proto::v1::ProbeKind::BindTcpHold,
                    )
                    .await;
                let bind_ok = matches!(&bind, Ok(r) if r.ok);
                if !bind_ok {
                    segments.push(tunnel_probe_seg(
                        from.node_id.clone(),
                        next.node_id.clone(),
                        bind.map(|r| relay_proto::v1::ProbeResult {
                            ok: false,
                            error: if r.error.is_empty() {
                                "bind 失败".into()
                            } else {
                                r.error
                            },
                            ..r
                        }),
                    ));
                    continue;
                }

                let connect = s
                    .registry
                    .probe(
                        &from.node_id,
                        format!("{next_ip}:{probe_port}"),
                        probe_timeout,
                    )
                    .await;
                segments.push(tunnel_probe_seg(
                    from.node_id.clone(),
                    next.node_id.clone(),
                    connect,
                ));
            }
        }
    }

    // 最后一层每个节点都做公网可达性
    let last_layer = layers.last().expect("layers non-empty checked above");
    let mut public_futs = Vec::new();
    for out in last_layer {
        for t in [
            "8.8.8.8:443",
            "1.1.1.1:443",
            "bing.com:443",
            "google.com:443",
        ] {
            let reg = s.registry.clone();
            let from = out.node_id.clone();
            let to = t.to_string();
            public_futs.push(async move {
                tunnel_probe_seg(
                    from.clone(),
                    to.clone(),
                    reg.probe(&from, to, probe_timeout).await,
                )
            });
        }
    }
    segments.extend(futures::future::join_all(public_futs).await);

    let resp = TunnelProbeResponse { segments };
    if resp.segments.iter().all(|s| s.ok) && !resp.segments.is_empty() {
        cache::set_json(&s.redis, &cache_key, &resp, 5).await;
    }
    Ok(Json(resp))
}

/// SSE 流式探测隧道：并行探测所有段，每完成一段立即推送
async fn probe_tunnel_stream(State(s): State<AppState>, Path(id): Path<i64>) -> Response {
    let rows: Vec<(i32, String, Vec<String>, i32, i32)> = match sqlx::query_as(
        "SELECT th.hop_index, th.node_id, n.server_ips,
                n.port_range_start, n.port_range_end
           FROM tunnel_hops th
           JOIN nodes n ON n.id = th.node_id
          WHERE th.tunnel_id = $1
          ORDER BY th.hop_index, th.node_id",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    {
        Ok(r) => r,
        Err(e) => return ApiError::from(e).into_response(),
    };

    if rows.is_empty() {
        return ApiError::new(StatusCode::NOT_FOUND, "隧道不存在或无节点").into_response();
    }

    struct LayerNode {
        node_id: String,
        ips: Vec<String>,
        range_start: i32,
        range_end: i32,
    }
    let mut layer_map: std::collections::BTreeMap<i32, Vec<LayerNode>> =
        std::collections::BTreeMap::new();
    for (hop, node_id, ips, rs, re) in rows {
        layer_map.entry(hop).or_default().push(LayerNode {
            node_id,
            ips,
            range_start: rs,
            range_end: re,
        });
    }
    let layers: Vec<Vec<LayerNode>> = layer_map.into_values().collect();

    let (tx, rx) = tokio::sync::mpsc::channel::<TunnelProbeSegment>(64);
    let probe_timeout = Duration::from_secs(1);
    let db = s.db.clone();
    let registry = s.registry.clone();

    tokio::spawn(async move {
        let mut futs = futures::stream::FuturesUnordered::<
            std::pin::Pin<Box<dyn std::future::Future<Output = TunnelProbeSegment> + Send>>,
        >::new();

        // 跨层段：全部并行
        for i in 0..layers.len().saturating_sub(1) {
            for from in &layers[i] {
                for next in &layers[i + 1] {
                    let db2 = db.clone();
                    let reg = registry.clone();
                    let from_id = from.node_id.clone();
                    let next_id = next.node_id.clone();
                    let next_ips = next.ips.clone();
                    let range_start = next.range_start;
                    let range_end = next.range_end;

                    futs.push(Box::pin(async move {
                        let next_ip = match next_ips.iter().find(|s| !s.trim().is_empty()) {
                            Some(ip) => ip.clone(),
                            None => {
                                return TunnelProbeSegment {
                                    from_node: from_id,
                                    to: next_id,
                                    ok: false,
                                    latency_us: 0,
                                    error: "节点无可用 IP".into(),
                                }
                            }
                        };

                        let probe_port = match crate::ports::pick_free_port(
                            &db2,
                            &next_id,
                            range_start + 1,
                            range_end,
                            &[],
                        )
                        .await
                        {
                            Ok(Some(p)) => p,
                            _ => {
                                return TunnelProbeSegment {
                                    from_node: from_id,
                                    to: next_id,
                                    ok: false,
                                    latency_us: 0,
                                    error: "节点无可用探测端口".into(),
                                }
                            }
                        };

                        let bind_hold = probe_timeout + Duration::from_secs(2);
                        let bind = reg
                            .probe_with_kind(
                                &next_id,
                                format!("[::]:{probe_port}"),
                                bind_hold,
                                relay_proto::v1::ProbeKind::BindTcpHold,
                            )
                            .await;
                        let bind_ok = matches!(&bind, Ok(r) if r.ok);
                        if !bind_ok {
                            return tunnel_probe_seg(
                                from_id,
                                next_id,
                                bind.map(|r| relay_proto::v1::ProbeResult {
                                    ok: false,
                                    error: if r.error.is_empty() {
                                        "bind 失败".into()
                                    } else {
                                        r.error
                                    },
                                    ..r
                                }),
                            );
                        }

                        let connect = reg
                            .probe(&from_id, format!("{next_ip}:{probe_port}"), probe_timeout)
                            .await;
                        tunnel_probe_seg(from_id, next_id, connect)
                    }));
                }
            }
        }

        // 最后一层公网可达性
        if let Some(last_layer) = layers.last() {
            for out in last_layer {
                for target in [
                    "8.8.8.8:443",
                    "1.1.1.1:443",
                    "bing.com:443",
                    "google.com:443",
                ] {
                    let reg = registry.clone();
                    let from = out.node_id.clone();
                    let to = target.to_string();
                    futs.push(Box::pin(async move {
                        tunnel_probe_seg(
                            from.clone(),
                            to.clone(),
                            reg.probe(&from, to, probe_timeout).await,
                        )
                    }));
                }
            }
        }

        while let Some(seg) = futures::StreamExt::next(&mut futs).await {
            if tx.send(seg).await.is_err() {
                break;
            }
        }
    });

    use tokio_stream::wrappers::ReceiverStream;
    let stream = ReceiverStream::new(rx).map(|seg: TunnelProbeSegment| {
        Ok::<_, Infallible>(match serde_json::to_string(&seg) {
            Ok(j) => Event::default().data(j),
            Err(_) => Event::default().comment("err"),
        })
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(3)))
        .into_response()
}

#[derive(Serialize, Deserialize)]
struct ForwardProbeHop {
    from_node: String,
    #[serde(default)]
    from_node_name: String,
    /// Empty when probing the final upstream (no next-layer node).
    #[serde(default)]
    to_node: String,
    #[serde(default)]
    to_node_name: String,
    target: String,
    ok: bool,
    latency_us: u64,
    error: String,
}

/// 单段探测描述
struct ProbeSegment {
    from_node: String,
    from_name: String,
    to_node: String,
    to_name: String,
    target: String,
}

/// 从数据库查询并构建探测段列表（被 probe_forward 和 probe_forward_stream 共用）
async fn build_probe_segments(
    db: &sqlx::PgPool,
    forward_id: i64,
) -> Result<Vec<ProbeSegment>, ApiError> {
    #[derive(sqlx::FromRow)]
    struct HopRow {
        hop_index: i32,
        node_id: String,
        node_name: String,
        server_ips: Vec<String>,
        listen_port: i32,
    }
    let hops: Vec<HopRow> = sqlx::query_as(
        "SELECT DISTINCT th.hop_index, th.node_id,
                n.hostname AS node_name,
                COALESCE(n.server_ips, '{}') AS server_ips,
                fp.listen_port
           FROM forwards f
           JOIN user_tunnels ut ON ut.id = f.user_tunnel_id
           JOIN tunnel_hops th ON th.tunnel_id = ut.tunnel_id
           JOIN nodes n ON n.id = th.node_id
           JOIN forward_ports fp ON fp.forward_id = f.id
                                AND fp.hop_index = th.hop_index
                                AND fp.node_id = th.node_id
          WHERE f.id = $1
          ORDER BY th.hop_index ASC, th.node_id ASC",
    )
    .bind(forward_id)
    .fetch_all(db)
    .await?;

    if hops.is_empty() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "转发不存在或无节点"));
    }

    let remote_addrs: Option<Vec<String>> =
        sqlx::query_scalar("SELECT remote_addrs FROM forwards WHERE id = $1")
            .bind(forward_id)
            .fetch_optional(db)
            .await?;
    let upstreams = remote_addrs
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "转发无上游地址"))?;

    // 按 hop_index 分层
    let mut layer_map: std::collections::BTreeMap<i32, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (idx, h) in hops.iter().enumerate() {
        layer_map.entry(h.hop_index).or_default().push(idx);
    }
    let layers: Vec<Vec<usize>> = layer_map.into_values().collect();

    let mut segments: Vec<ProbeSegment> = Vec::new();
    for (i, layer_indices) in layers.iter().enumerate() {
        let is_last = i + 1 == layers.len();
        for &from_idx in layer_indices {
            let from = &hops[from_idx];
            if is_last {
                for upstream in &upstreams {
                    segments.push(ProbeSegment {
                        from_node: from.node_id.clone(),
                        from_name: from.node_name.clone(),
                        to_node: String::new(),
                        to_name: String::new(),
                        target: upstream.clone(),
                    });
                }
            } else {
                let next_layer_indices = &layers[i + 1];
                for &next_idx in next_layer_indices {
                    let next = &hops[next_idx];
                    let ip = next
                        .server_ips
                        .iter()
                        .find(|s| !s.trim().is_empty())
                        .cloned()
                        .unwrap_or_default();
                    let target = format!("{}:{}", ip, next.listen_port);
                    segments.push(ProbeSegment {
                        from_node: from.node_id.clone(),
                        from_name: from.node_name.clone(),
                        to_node: next.node_id.clone(),
                        to_name: next.node_name.clone(),
                        target,
                    });
                }
            }
        }
    }

    Ok(segments)
}

async fn probe_forward(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<ForwardProbeHop>>> {
    authorize_forward_access(&s.db, &claims, id).await?;

    // 鉴权之后再查缓存。v2 key 因数据形态变化（多节点层、to_node 字段）必须升级。
    let cache_key = format!("probe:forward:v2:{}", id);
    if let Some(cached) = cache::get_json::<Vec<ForwardProbeHop>>(&s.redis, &cache_key).await {
        return Ok(Json(cached));
    }

    let segments = build_probe_segments(&s.db, id).await?;

    let timeout = std::time::Duration::from_secs(1);
    let futures: Vec<_> = segments
        .into_iter()
        .map(|seg| {
            let registry = s.registry.clone();
            async move {
                let t = seg.target.clone();
                let result = registry.probe(&seg.from_node, t.clone(), timeout).await;
                match result {
                    Ok(r) => ForwardProbeHop {
                        from_node: seg.from_node,
                        from_node_name: seg.from_name,
                        to_node: seg.to_node,
                        to_node_name: seg.to_name,
                        target: t,
                        ok: r.ok,
                        latency_us: r.latency_us,
                        error: r.error,
                    },
                    Err(crate::registry::ProbeError::NodeOffline) => ForwardProbeHop {
                        from_node: seg.from_node,
                        from_node_name: seg.from_name,
                        to_node: seg.to_node,
                        to_node_name: seg.to_name,
                        target: t,
                        ok: false,
                        latency_us: 0,
                        error: "节点未连接".into(),
                    },
                    Err(crate::registry::ProbeError::Timeout) => ForwardProbeHop {
                        from_node: seg.from_node,
                        from_node_name: seg.from_name,
                        to_node: seg.to_node,
                        to_node_name: seg.to_name,
                        target: t,
                        ok: false,
                        latency_us: 0,
                        error: "探测超时".into(),
                    },
                }
            }
        })
        .collect();

    let results = futures::future::join_all(futures).await;
    if results.iter().all(|h| h.ok) && !results.is_empty() {
        cache::set_json(&s.redis, &cache_key, &results, 5).await;
    }
    Ok(Json(results))
}

/// SSE 流式探测端点：每探测完一段就立刻推送该段结果
async fn probe_forward_stream(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
) -> Response {
    // 鉴权
    if let Err(e) = authorize_forward_access(&s.db, &claims, id).await {
        return e.into_response();
    }
    // 构建探测段
    let segments = match build_probe_segments(&s.db, id).await {
        Ok(segs) => segs,
        Err(e) => return e.into_response(),
    };
    // mpsc channel，缓冲足够大
    let (tx, rx) = tokio::sync::mpsc::channel::<ForwardProbeHop>(32);
    // FuturesUnordered 并发探测，先完先发
    let registry = s.registry.clone();
    let timeout = std::time::Duration::from_secs(1);
    tokio::spawn(async move {
        let mut futs = futures::stream::FuturesUnordered::new();
        for seg in segments {
            let reg = registry.clone();
            futs.push(async move {
                let t = seg.target.clone();
                let result = reg.probe(&seg.from_node, t.clone(), timeout).await;
                match result {
                    Ok(r) => ForwardProbeHop {
                        from_node: seg.from_node,
                        from_node_name: seg.from_name,
                        to_node: seg.to_node,
                        to_node_name: seg.to_name,
                        target: t,
                        ok: r.ok,
                        latency_us: r.latency_us,
                        error: r.error,
                    },
                    Err(crate::registry::ProbeError::NodeOffline) => ForwardProbeHop {
                        from_node: seg.from_node,
                        from_node_name: seg.from_name,
                        to_node: seg.to_node,
                        to_node_name: seg.to_name,
                        target: t,
                        ok: false,
                        latency_us: 0,
                        error: "节点未连接".into(),
                    },
                    Err(crate::registry::ProbeError::Timeout) => ForwardProbeHop {
                        from_node: seg.from_node,
                        from_node_name: seg.from_name,
                        to_node: seg.to_node,
                        to_node_name: seg.to_name,
                        target: t,
                        ok: false,
                        latency_us: 0,
                        error: "探测超时".into(),
                    },
                }
            });
        }
        while let Some(hop) = futures::StreamExt::next(&mut futs).await {
            if tx.send(hop).await.is_err() {
                break;
            }
        }
    });
    // SSE 流
    use tokio_stream::wrappers::ReceiverStream;
    let stream = ReceiverStream::new(rx).map(|hop: ForwardProbeHop| {
        Ok::<_, Infallible>(match serde_json::to_string(&hop) {
            Ok(j) => Event::default().data(j),
            Err(_) => Event::default().comment("err"),
        })
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(3)))
        .into_response()
}

async fn nodes_for_tunnel(db: &sqlx::PgPool, tunnel_id: i64) -> sqlx::Result<Vec<String>> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT DISTINCT node_id FROM tunnel_hops WHERE tunnel_id = $1")
            .bind(tunnel_id)
            .fetch_all(db)
            .await?;
    Ok(rows.into_iter().map(|(n,)| n).collect())
}

// ---------- User-Tunnels ----------

#[derive(Deserialize)]
pub struct UserTunnelQuery {
    #[serde(default, with = "crate::snowflake::as_str_opt")]
    pub user_id: Option<i64>,
    #[serde(default, with = "crate::snowflake::as_str_opt")]
    pub tunnel_id: Option<i64>,
}

async fn list_user_tunnels(
    State(s): State<AppState>,
    Query(q): Query<UserTunnelQuery>,
) -> ApiResult<Json<Vec<UserTunnelView>>> {
    list_user_tunnels_inner(&s.db, q.user_id, q.tunnel_id).await
}

async fn list_user_tunnels_inner(
    db: &sqlx::PgPool,
    user_id: Option<i64>,
    tunnel_id: Option<i64>,
) -> ApiResult<Json<Vec<UserTunnelView>>> {
    type Row = (
        i64,
        i64,
        i64,
        i64,
        i64,
        Option<DateTime<Utc>>,
        bool,
        DateTime<Utc>,
        DateTime<Utc>,
        String,
        String,
        Vec<String>,
        i64,
        i64,
        i64,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT ut.id, ut.user_id, ut.tunnel_id, ut.flow_limit_bytes, ut.speed_limit_kbps,
                ut.expires_at, ut.enabled, ut.created_at, ut.updated_at,
                u.username, t.name, t.protocols,
                COALESCE(SUM(f.in_flow_bytes),0)::BIGINT,
                COALESCE(SUM(f.out_flow_bytes),0)::BIGINT,
                COALESCE(COUNT(f.id),0)::BIGINT
           FROM user_tunnels ut
           JOIN users u ON u.id = ut.user_id
           JOIN tunnels t ON t.id = ut.tunnel_id
           LEFT JOIN forwards f ON f.user_tunnel_id = ut.id
          WHERE ($1::BIGINT IS NULL OR ut.user_id = $1)
            AND ($2::BIGINT IS NULL OR ut.tunnel_id = $2)
          GROUP BY ut.id, u.username, t.name, t.protocols
          ORDER BY ut.created_at DESC",
    )
    .bind(user_id)
    .bind(tunnel_id)
    .fetch_all(db)
    .await?;
    let out = rows
        .into_iter()
        .map(
            |(
                id,
                uid,
                tid,
                limit,
                speed,
                exp,
                en,
                cre,
                upd,
                username,
                tname,
                proto,
                inb,
                outb,
                fc,
            )| {
                UserTunnelView {
                    user_tunnel: UserTunnel {
                        id,
                        user_id: uid,
                        tunnel_id: tid,
                        flow_limit_bytes: limit,
                        speed_limit_kbps: speed,
                        expires_at: exp,
                        enabled: en,
                        created_at: cre,
                        updated_at: upd,
                    },
                    username,
                    tunnel_name: tname,
                    tunnel_protocols: proto,
                    in_flow_bytes: inb,
                    out_flow_bytes: outb,
                    forward_count: fc,
                }
            },
        )
        .collect();
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct CreateUserTunnelReq {
    #[serde(with = "crate::snowflake::as_str")]
    pub user_id: i64,
    #[serde(with = "crate::snowflake::as_str")]
    pub tunnel_id: i64,
    /// Convenience: GB. If `flow_limit_bytes` provided takes precedence.
    #[serde(default)]
    pub flow_limit_gb: Option<f64>,
    #[serde(default)]
    pub flow_limit_bytes: Option<i64>,
    /// Bandwidth cap in KB/s (0 = unlimited).
    #[serde(default)]
    pub speed_limit_kbps: Option<i64>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

async fn create_user_tunnel(
    State(s): State<AppState>,
    Json(req): Json<CreateUserTunnelReq>,
) -> ApiResult<Json<UserTunnel>> {
    let limit = req
        .flow_limit_bytes
        .or_else(|| {
            req.flow_limit_gb
                .map(|g| (g * 1024.0 * 1024.0 * 1024.0).round() as i64)
        })
        .unwrap_or(0);
    if limit < 0 {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "flow_limit 不能为负",
        ));
    }
    let speed = req.speed_limit_kbps.unwrap_or(0);
    if speed < 0 {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "speed_limit_kbps 不能为负",
        ));
    }
    let row: UserTunnel = sqlx::query_as(
        "INSERT INTO user_tunnels (id, user_id, tunnel_id, flow_limit_bytes, speed_limit_kbps, expires_at, enabled)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *",
    )
    .bind(crate::snowflake::next_id())
    .bind(req.user_id)
    .bind(req.tunnel_id)
    .bind(limit)
    .bind(speed)
    .bind(req.expires_at)
    .bind(req.enabled)
    .fetch_one(&s.db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => {
            ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "用户已绑定该隧道")
        }
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23503") => {
            ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "用户或隧道不存在")
        }
        _ => ApiError::from(e),
    })?;
    Ok(Json(row))
}

#[derive(Deserialize)]
pub struct UpdateUserTunnelReq {
    pub flow_limit_bytes: Option<i64>,
    pub flow_limit_gb: Option<f64>,
    pub speed_limit_kbps: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    pub expires_at: Option<Option<DateTime<Utc>>>,
    pub enabled: Option<bool>,
}

async fn update_user_tunnel(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateUserTunnelReq>,
) -> ApiResult<Json<UserTunnel>> {
    let limit = req.flow_limit_bytes.or_else(|| {
        req.flow_limit_gb
            .map(|g| (g * 1024.0 * 1024.0 * 1024.0).round() as i64)
    });
    let prev_enabled: Option<(bool,)> =
        sqlx::query_as("SELECT enabled FROM user_tunnels WHERE id = $1")
            .bind(id)
            .fetch_optional(&s.db)
            .await?;
    let row: UserTunnel = sqlx::query_as(
        "UPDATE user_tunnels SET
            flow_limit_bytes  = COALESCE($2, flow_limit_bytes),
            speed_limit_kbps  = COALESCE($3, speed_limit_kbps),
            expires_at        = CASE WHEN $4::bool THEN $5 ELSE expires_at END,
            enabled           = COALESCE($6, enabled),
            updated_at        = now()
          WHERE id = $1 RETURNING *",
    )
    .bind(id)
    .bind(limit)
    .bind(req.speed_limit_kbps)
    .bind(req.expires_at.is_some())
    .bind(req.expires_at.flatten())
    .bind(req.enabled)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "用户隧道不存在"))?;

    // Reason updates + push
    if let Some(new_enabled) = req.enabled {
        let was = prev_enabled.map(|(b,)| b).unwrap_or(true);
        if was != new_enabled {
            let forwards: Vec<(i64,)> =
                sqlx::query_as("SELECT id FROM forwards WHERE user_tunnel_id = $1")
                    .bind(id)
                    .fetch_all(&s.db)
                    .await?;
            for (fid,) in &forwards {
                if new_enabled {
                    let _ = crate::pause::clear_pause_reason(
                        &s.db,
                        *fid,
                        crate::pause::REASON_USER_TUNNEL_DISABLED,
                    )
                    .await;
                } else {
                    let _ = crate::pause::write_pause_reason(
                        &s.db,
                        *fid,
                        crate::pause::REASON_USER_TUNNEL_DISABLED,
                    )
                    .await;
                }
            }
            bump_and_push_forwards(&s, &forwards.iter().map(|(f,)| *f).collect::<Vec<_>>()).await;
        }
    }
    if req.expires_at.is_some() {
        crate::scheduler::kick(s.db.clone(), s.registry.clone());
    }
    Ok(Json(row))
}

#[derive(Deserialize)]
pub struct DeleteUserTunnelQuery {
    #[serde(default)]
    pub cascade: bool,
}

async fn delete_user_tunnel(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<DeleteUserTunnelQuery>,
) -> ApiResult<StatusCode> {
    let count: (i64,) = sqlx::query_as("SELECT count(*) FROM forwards WHERE user_tunnel_id = $1")
        .bind(id)
        .fetch_one(&s.db)
        .await?;
    if count.0 > 0 && !q.cascade {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "该用户隧道下仍有转发；请先删除或加 ?cascade=true",
        ));
    }
    // Collect node set first (so we can push afterwards).
    let nodes: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT fp.node_id FROM forward_ports fp
           JOIN forwards f ON f.id = fp.forward_id
          WHERE f.user_tunnel_id = $1",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await?;

    let mut tx = s.db.begin().await?;
    sqlx::query("DELETE FROM forwards WHERE user_tunnel_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let res = sqlx::query("DELETE FROM user_tunnels WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "用户隧道不存在"));
    }
    for (nid,) in &nodes {
        s.registry.push_config(&s.db, nid).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn list_my_user_tunnels(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<Vec<UserTunnelView>>> {
    let id = caller_id(&claims)?;
    list_user_tunnels_inner(&s.db, Some(id), None).await
}

// ---------- Forwards ----------

async fn authorize_forward_access(
    db: &sqlx::PgPool,
    claims: &Claims,
    forward_id: i64,
) -> Result<(), ApiError> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT ut.user_id FROM forwards f
           JOIN user_tunnels ut ON ut.id = f.user_tunnel_id
          WHERE f.id = $1",
    )
    .bind(forward_id)
    .fetch_optional(db)
    .await?;
    let (owner_id,) = row.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "资源不存在"))?;
    if claims.role == "admin" {
        return Ok(());
    }
    let me = caller_id(claims)?;
    if me != owner_id {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "资源不存在"));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct ForwardQuery {
    #[serde(default, with = "crate::snowflake::as_str_opt")]
    pub user_id: Option<i64>,
    #[serde(default, with = "crate::snowflake::as_str_opt")]
    pub user_tunnel_id: Option<i64>,
    #[serde(default, with = "crate::snowflake::as_str_opt")]
    pub tunnel_id: Option<i64>,
}

async fn list_forwards(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<ForwardQuery>,
) -> ApiResult<Json<Vec<ForwardView>>> {
    let is_admin = claims.role == "admin";
    let scope_user: Option<i64> = if is_admin {
        q.user_id
    } else {
        Some(caller_id(&claims)?)
    };
    let rows: Vec<ForwardRowEx> = sqlx::query_as(
        "SELECT f.*, ut.user_id, u.username, ut.tunnel_id, t.name AS tunnel_name, t.protocols, t.in_ip
           FROM forwards f
           JOIN user_tunnels ut ON ut.id = f.user_tunnel_id
           JOIN users u ON u.id = ut.user_id
           JOIN tunnels t ON t.id = ut.tunnel_id
          WHERE ($1::BIGINT IS NULL OR ut.user_id = $1)
            AND ($2::BIGINT IS NULL OR f.user_tunnel_id = $2)
            AND ($3::BIGINT IS NULL OR ut.tunnel_id = $3)
          ORDER BY f.created_at DESC",
    )
    .bind(scope_user)
    .bind(q.user_tunnel_id)
    .bind(q.tunnel_id)
    .fetch_all(&s.db)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(build_forward_view(&s.db, &s.series, &s.active_conns, r).await?);
    }
    Ok(Json(out))
}

#[derive(sqlx::FromRow)]
struct ForwardRowEx {
    #[sqlx(flatten)]
    f: Forward,
    user_id: i64,
    username: String,
    tunnel_id: i64,
    tunnel_name: String,
    protocols: Vec<String>,
    in_ip: String,
}

async fn build_forward_view(
    db: &sqlx::PgPool,
    series: &crate::series::SeriesStore,
    active_conns: &crate::state::ActiveConnStore,
    r: ForwardRowEx,
) -> Result<ForwardView, ApiError> {
    let ports: Vec<ForwardPort> = sqlx::query_as(
        "SELECT forward_id, hop_index, node_id, protocol, listen_port
           FROM forward_ports WHERE forward_id = $1 ORDER BY hop_index",
    )
    .bind(r.f.id)
    .fetch_all(db)
    .await?;
    let (effective_enabled, pause_reasons) =
        crate::pause::compute_effective_enabled(db, r.f.id).await?;

    // Entry layer (hop 0) may host multiple nodes (DNS-LB). Aggregate active
    // connections across them and expose all entry addresses.
    let entry_node_ids: Vec<String> = ports
        .iter()
        .filter(|p| p.hop_index == 0)
        .map(|p| p.node_id.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let entry_listen_port: Option<i32> = ports
        .iter()
        .find(|p| p.hop_index == 0)
        .map(|p| p.listen_port);

    let active_connections = if entry_node_ids.is_empty() {
        0
    } else {
        series
            .latest_forward_active_many(&entry_node_ids, &r.f.id.to_string())
            .await
    };

    let mut entry_addrs: Vec<String> = Vec::new();
    if let Some(port) = entry_listen_port {
        if !r.in_ip.is_empty() {
            // 隧道指定了入口地址（IP 或域名），所有入口节点统一用它
            entry_addrs.push(format!("{}:{port}", r.in_ip));
        } else {
            for nid in &entry_node_ids {
                let ips: Option<Vec<String>> =
                    sqlx::query_scalar("SELECT server_ips FROM nodes WHERE id = $1")
                        .bind(nid)
                        .fetch_optional(db)
                        .await?;
                if let Some(first) = ips.and_then(|v| v.into_iter().find(|s| !s.trim().is_empty()))
                {
                    entry_addrs.push(format!("{first}:{port}"));
                }
            }
        }
    }
    let entry_addr = entry_addrs.first().cloned();

    // best/least_latency 策略时查询当前最优出口（最近 5 分钟探测得分最低的 upstream）
    let best_exit_addr: Option<String> =
        if matches!(r.f.lb_strategy.as_str(), "best" | "least_latency") {
            sqlx::query_scalar(
                "SELECT upstream_addr
               FROM forward_probe_samples
              WHERE forward_id = $1
                AND probed_at >= now() - INTERVAL '5 minutes'
              GROUP BY upstream_addr
              ORDER BY AVG(COALESCE(latency_us::float8, 5000000)) ASC
              LIMIT 1",
            )
            .bind(r.f.id)
            .fetch_optional(db)
            .await
            .unwrap_or(None)
        } else {
            None
        };

    let active_client_ips = active_conns.get(&r.f.id.to_string()).await;

    // 所有跳节点都已回执当前 tunnels_version 则视为已同步
    let synced: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS (
             SELECT 1 FROM forward_ports fp
             JOIN nodes n ON n.id = fp.node_id
             WHERE fp.forward_id = $1
               AND n.tunnels_version != n.last_applied_version
         ) AND EXISTS (
             SELECT 1 FROM forward_ports WHERE forward_id = $1
         )",
    )
    .bind(r.f.id)
    .fetch_one(db)
    .await
    .unwrap_or(false);

    Ok(ForwardView {
        forward: r.f,
        user_id: r.user_id,
        username: r.username,
        tunnel_id: r.tunnel_id,
        tunnel_name: r.tunnel_name,
        protocols: r.protocols,
        ports,
        effective_enabled,
        pause_reasons,
        active_connections,
        entry_addr,
        entry_addrs,
        best_exit_addr,
        active_client_ips,
        synced,
    })
}

#[derive(Deserialize)]
pub struct CreateForwardReq {
    #[serde(with = "crate::snowflake::as_str")]
    pub tunnel_id: i64,
    pub name: String,
    /// 0 or omitted = auto-allocate from entry node range.
    #[serde(default)]
    pub in_port: Option<i32>,
    pub remote_addrs: Vec<String>,
    #[serde(default)]
    pub lb_strategy: Option<String>,
    #[serde(default)]
    pub max_connections: Option<i32>,
    #[serde(default)]
    pub allow_cidrs: Option<Vec<String>>,
    #[serde(default)]
    pub deny_cidrs: Option<Vec<String>>,
}

fn validate_remote_addrs(addrs: &[String]) -> Result<(), ApiError> {
    if addrs.is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "remote_addrs 不能为空",
        ));
    }
    for a in addrs {
        let colon = a.rfind(':').ok_or_else(|| {
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("remote_addrs 包含非法地址 {a}：需 host:port"),
            )
        })?;
        let host = &a[..colon];
        let port = &a[colon + 1..];
        if host.is_empty() {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("remote_addrs 缺少主机：{a}"),
            ));
        }
        port.parse::<u16>().map_err(|_| {
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("remote_addrs 端口非法：{a}"),
            )
        })?;
    }
    Ok(())
}

#[derive(Serialize)]
struct CreateForwardResp {
    #[serde(flatten)]
    view: ForwardView,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    port_warnings: Vec<String>,
}

async fn create_forward(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateForwardReq>,
) -> ApiResult<Json<CreateForwardResp>> {
    if req.name.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "name 不能为空",
        ));
    }
    validate_remote_addrs(&req.remote_addrs)?;
    if let Some(lb) = req.lb_strategy.as_deref() {
        validate_lb(lb)?;
    }
    // 确认隧道存在
    let tunnel_row: Option<(Vec<String>,)> =
        sqlx::query_as("SELECT protocols FROM tunnels WHERE id = $1")
            .bind(req.tunnel_id)
            .fetch_optional(&s.db)
            .await?;
    let (protocols,) =
        tunnel_row.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "隧道不存在"))?;

    let user_id = caller_id(&claims)?;

    // 非管理员必须持有 enabled 的 user_tunnel 才能使用该隧道
    if claims.role != "admin" {
        let allowed: Option<(i64,)> = sqlx::query_as(
            "SELECT id FROM user_tunnels
              WHERE user_id = $1 AND tunnel_id = $2 AND enabled = true",
        )
        .bind(user_id)
        .bind(req.tunnel_id)
        .fetch_optional(&s.db)
        .await?;
        if allowed.is_none() {
            return Err(ApiError::new(StatusCode::FORBIDDEN, "无权使用此隧道"));
        }
    }

    // 检查转发数量是否已达上限
    let limit_row: Option<(i32,)> = sqlx::query_as(
        "SELECT g.forward_limit FROM group_members gm
           JOIN user_groups g ON g.id = gm.group_id
          WHERE gm.user_id = $1
          LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(&s.db)
    .await?;
    let forward_limit = limit_row.map(|(l,)| l).unwrap_or(0);
    if forward_limit > 0 {
        let (current_count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM forwards f
               JOIN user_tunnels ut ON ut.id = f.user_tunnel_id
              WHERE ut.user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&s.db)
        .await?;
        if current_count >= forward_limit as i64 {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "已达到转发数量上限",
            ));
        }
    }

    let user_tunnel_id: i64 = sqlx::query_scalar(
        "INSERT INTO user_tunnels (id, user_id, tunnel_id)
         VALUES ($1, $2, $3)
         ON CONFLICT (user_id, tunnel_id) DO UPDATE SET updated_at = now()
         RETURNING id",
    )
    .bind(crate::snowflake::next_id())
    .bind(user_id)
    .bind(req.tunnel_id)
    .fetch_one(&s.db)
    .await?;

    let tunnel_id = req.tunnel_id;

    let mut tx = s.db.begin().await?;
    let forward_id = crate::snowflake::next_id();
    let forward: Forward = sqlx::query_as(
        "INSERT INTO forwards
            (id, user_tunnel_id, name, in_port, remote_addrs, lb_strategy,
             max_connections, allow_cidrs, deny_cidrs)
         VALUES ($1,$2,$3,COALESCE($4,1),$5,COALESCE($6,'round_robin'),
                 COALESCE($7,0), COALESCE($8,'{}'::TEXT[]), COALESCE($9,'{}'::TEXT[]))
         RETURNING *",
    )
    .bind(forward_id)
    .bind(user_tunnel_id)
    .bind(&req.name)
    // Use 1 as placeholder when auto-allocating; we'll overwrite from forward_ports below.
    .bind(req.in_port.filter(|p| *p > 0).unwrap_or(1))
    .bind(&req.remote_addrs)
    .bind(req.lb_strategy.as_deref())
    .bind(req.max_connections)
    .bind(req.allow_cidrs.as_deref())
    .bind(req.deny_cidrs.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_err)?;

    let proto_slice: Vec<&str> = protocols.iter().map(|s| s.as_str()).collect();
    crate::ports::allocate_forward_ports(&mut tx, forward.id, tunnel_id, &proto_slice, req.in_port)
        .await?;

    // Sync forwards.in_port from the entry forward_port row.
    // 双协议时同 hop 各 protocol 行 listen_port 相同（DB 触发器保证），DISTINCT 拿一行。
    let (entry_port,): (i32,) = sqlx::query_as(
        "SELECT DISTINCT listen_port FROM forward_ports
          WHERE forward_id = $1 AND hop_index = 0",
    )
    .bind(forward.id)
    .fetch_one(&mut *tx)
    .await?;
    let forward: Forward =
        sqlx::query_as("UPDATE forwards SET in_port = $2 WHERE id = $1 RETURNING *")
            .bind(forward.id)
            .bind(entry_port)
            .fetch_one(&mut *tx)
            .await?;

    tx.commit().await?;

    // 若该 user_tunnel 已超流量配额，新建转发立即继承暂停状态
    let quota_exceeded: Option<(i64,)> = sqlx::query_as(
        "SELECT f.id FROM forwards f
           JOIN forward_pause_reasons r ON r.forward_id = f.id
          WHERE f.user_tunnel_id = $1
            AND r.reason = $2
          LIMIT 1",
    )
    .bind(user_tunnel_id)
    .bind(crate::pause::REASON_TUNNEL_QUOTA_EXCEEDED)
    .fetch_optional(&s.db)
    .await?;
    if quota_exceeded.is_some() {
        let _ = crate::pause::write_pause_reason(
            &s.db,
            forward.id,
            crate::pause::REASON_TUNNEL_QUOTA_EXCEEDED,
        )
        .await;
    }

    // 探测并修复各跳端口；失败时删除刚创建的 forward 并返回错误。
    let port_warnings = match probe_and_fix_ports(&s, forward.id, &protocols).await {
        Ok(w) => w,
        Err(e) => {
            let _ = sqlx::query("DELETE FROM forwards WHERE id = $1")
                .bind(forward.id)
                .execute(&s.db)
                .await;
            return Err(e);
        }
    };

    // 端口修复后再推送配置，downstream-first。SELECT DISTINCT 防止同一
    // 节点在多 protocol / 同层情况下被重复推送。按 hop_index DESC 让下游
    // 先就绪后再让上游下发新 upstream_addrs。
    let push_targets: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT node_id FROM forward_ports
           WHERE forward_id = $1
           ORDER BY node_id",
    )
    .bind(forward.id)
    .fetch_all(&s.db)
    .await?;
    let mut ordered: Vec<(i32, String)> = Vec::new();
    for (nid,) in &push_targets {
        let max_hop: (i32,) = sqlx::query_as(
            "SELECT MAX(hop_index) FROM forward_ports
              WHERE forward_id = $1 AND node_id = $2",
        )
        .bind(forward.id)
        .bind(nid)
        .fetch_one(&s.db)
        .await?;
        ordered.push((max_hop.0, nid.clone()));
    }
    ordered.sort_by_key(|(h, _)| -*h);
    for (_, nid) in &ordered {
        s.registry.push_config(&s.db, nid).await;
    }

    let row = fetch_forward_row(&s.db, forward.id).await?;
    Ok(Json(CreateForwardResp {
        view: build_forward_view(&s.db, &s.series, &s.active_conns, row).await?,
        port_warnings,
    }))
}

async fn fetch_forward_row(db: &sqlx::PgPool, id: i64) -> Result<ForwardRowEx, ApiError> {
    let r: Option<ForwardRowEx> = sqlx::query_as(
        "SELECT f.*, ut.user_id, u.username, ut.tunnel_id, t.name AS tunnel_name, t.protocols, t.in_ip
           FROM forwards f
           JOIN user_tunnels ut ON ut.id = f.user_tunnel_id
           JOIN users u ON u.id = ut.user_id
           JOIN tunnels t ON t.id = ut.tunnel_id
          WHERE f.id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    r.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "资源不存在"))
}

async fn get_forward(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
) -> ApiResult<Json<ForwardView>> {
    authorize_forward_access(&s.db, &claims, id).await?;
    let row = fetch_forward_row(&s.db, id).await?;
    Ok(Json(
        build_forward_view(&s.db, &s.series, &s.active_conns, row).await?,
    ))
}

#[derive(Deserialize)]
pub struct UpdateForwardReq {
    #[serde(default, with = "crate::snowflake::as_str_opt")]
    pub tunnel_id: Option<i64>,
    pub name: Option<String>,
    pub in_port: Option<i32>,
    pub remote_addrs: Option<Vec<String>>,
    pub lb_strategy: Option<String>,
    pub max_connections: Option<i32>,
    pub allow_cidrs: Option<Vec<String>>,
    pub deny_cidrs: Option<Vec<String>>,
}

async fn update_forward(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateForwardReq>,
) -> ApiResult<Json<ForwardView>> {
    authorize_forward_access(&s.db, &claims, id).await?;
    if let Some(addrs) = &req.remote_addrs {
        validate_remote_addrs(addrs)?;
    }
    if let Some(lb) = req.lb_strategy.as_deref() {
        validate_lb(lb)?;
    }

    if let Some(new_tunnel_id) = req.tunnel_id {
        // 换隧道：重新分配端口，通知新旧节点。
        let tunnel_row: Option<(Vec<String>,)> =
            sqlx::query_as("SELECT protocols FROM tunnels WHERE id = $1")
                .bind(new_tunnel_id)
                .fetch_optional(&s.db)
                .await?;
        let (protocols,) =
            tunnel_row.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "隧道不存在"))?;

        let user_id = caller_id(&claims)?;
        if claims.role != "admin" {
            let allowed: Option<(i64,)> = sqlx::query_as(
                "SELECT id FROM user_tunnels
                  WHERE user_id = $1 AND tunnel_id = $2 AND enabled = true",
            )
            .bind(user_id)
            .bind(new_tunnel_id)
            .fetch_optional(&s.db)
            .await?;
            if allowed.is_none() {
                return Err(ApiError::new(StatusCode::FORBIDDEN, "无权使用此隧道"));
            }
        }

        // 记录旧节点，用于隧道迁移完成后推送"删除"配置。
        let old_nodes: Vec<(String,)> =
            sqlx::query_as("SELECT DISTINCT node_id FROM forward_ports WHERE forward_id = $1")
                .bind(id)
                .fetch_all(&s.db)
                .await?;

        let user_tunnel_id: i64 = sqlx::query_scalar(
            "INSERT INTO user_tunnels (id, user_id, tunnel_id)
             VALUES ($1, $2, $3)
             ON CONFLICT (user_id, tunnel_id) DO UPDATE SET updated_at = now()
             RETURNING id",
        )
        .bind(crate::snowflake::next_id())
        .bind(user_id)
        .bind(new_tunnel_id)
        .fetch_one(&s.db)
        .await?;

        let proto_slice: Vec<&str> = protocols.iter().map(|s| s.as_str()).collect();
        let mut tx = s.db.begin().await?;

        sqlx::query("DELETE FROM forward_ports WHERE forward_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE forwards SET user_tunnel_id = $2, in_port = 1 WHERE id = $1")
            .bind(id)
            .bind(user_tunnel_id)
            .execute(&mut *tx)
            .await?;

        crate::ports::allocate_forward_ports(&mut tx, id, new_tunnel_id, &proto_slice, req.in_port)
            .await?;

        let (entry_port,): (i32,) = sqlx::query_as(
            "SELECT DISTINCT listen_port FROM forward_ports
              WHERE forward_id = $1 AND hop_index = 0",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("UPDATE forwards SET in_port = $2 WHERE id = $1")
            .bind(id)
            .bind(entry_port)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;

        // 旧节点推送（会发现该 forward 消失），新节点推送（获得新 forward）。
        for (nid,) in &old_nodes {
            s.registry.push_config(&s.db, nid).await;
        }
        let new_nodes: Vec<(String,)> =
            sqlx::query_as("SELECT DISTINCT node_id FROM forward_ports WHERE forward_id = $1")
                .bind(id)
                .fetch_all(&s.db)
                .await
                .unwrap_or_default();
        for (nid,) in &new_nodes {
            s.registry.push_config(&s.db, nid).await;
        }
    } else if let Some(new_port) = req.in_port {
        // 仅改端口（隧道不变）。
        let (cur_port,): (i32,) = sqlx::query_as("SELECT in_port FROM forwards WHERE id = $1")
            .bind(id)
            .fetch_one(&s.db)
            .await?;
        if new_port != cur_port {
            let (protocols,): (Vec<String>,) = sqlx::query_as(
                "SELECT t.protocols FROM forwards f
                   JOIN user_tunnels ut ON ut.id = f.user_tunnel_id
                   JOIN tunnels t ON t.id = ut.tunnel_id
                  WHERE f.id = $1",
            )
            .bind(id)
            .fetch_one(&s.db)
            .await?;
            let proto_refs: Vec<&str> = protocols.iter().map(|s| s.as_str()).collect();
            crate::ports::reallocate_layer_port(&s.db, id, 0, &proto_refs, Some(new_port)).await?;
        }
    }

    sqlx::query(
        "UPDATE forwards SET
            name            = COALESCE($2, name),
            remote_addrs    = COALESCE($3, remote_addrs),
            lb_strategy     = COALESCE($4, lb_strategy),
            max_connections = COALESCE($5, max_connections),
            allow_cidrs     = COALESCE($6, allow_cidrs),
            deny_cidrs      = COALESCE($7, deny_cidrs),
            updated_at      = now()
          WHERE id = $1",
    )
    .bind(id)
    .bind(req.name.as_deref())
    .bind(req.remote_addrs.as_deref())
    .bind(req.lb_strategy.as_deref())
    .bind(req.max_connections)
    .bind(req.allow_cidrs.as_deref())
    .bind(req.deny_cidrs.as_deref())
    .execute(&s.db)
    .await?;
    bump_and_push_forwards(&s, &[id]).await;
    let row = fetch_forward_row(&s.db, id).await?;
    Ok(Json(
        build_forward_view(&s.db, &s.series, &s.active_conns, row).await?,
    ))
}

async fn delete_forward(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    authorize_forward_access(&s.db, &claims, id).await?;
    let nodes: Vec<(String,)> =
        sqlx::query_as("SELECT DISTINCT node_id FROM forward_ports WHERE forward_id = $1")
            .bind(id)
            .fetch_all(&s.db)
            .await?;
    let mut tx = s.db.begin().await?;
    let res = sqlx::query("DELETE FROM forwards WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "资源不存在"));
    }
    tx.commit().await?;
    for (nid,) in &nodes {
        s.registry.push_config(&s.db, nid).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn pause_forward(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    authorize_forward_access(&s.db, &claims, id).await?;
    sqlx::query("UPDATE forwards SET desired_enabled = FALSE, updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(&s.db)
        .await?;
    bump_and_push_forwards(&s, &[id]).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn resume_forward(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    authorize_forward_access(&s.db, &claims, id).await?;
    sqlx::query("UPDATE forwards SET desired_enabled = TRUE, updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(&s.db)
        .await?;
    bump_and_push_forwards(&s, &[id]).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn redeploy_forward(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    authorize_forward_access(&s.db, &claims, id).await?;
    sqlx::query(
        "UPDATE forwards SET deploy_generation = deploy_generation + 1,
                              last_deploy_error = NULL,
                              updated_at = now()
          WHERE id = $1",
    )
    .bind(id)
    .execute(&s.db)
    .await?;
    let _ = crate::pause::clear_pause_reason(&s.db, id, crate::pause::REASON_DEPLOY_FAILED).await;
    bump_and_push_forwards(&s, &[id]).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct BatchReq {
    pub ids: Vec<i64>,
}

async fn batch_delete_forwards(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<BatchReq>,
) -> ApiResult<StatusCode> {
    for id in &req.ids {
        authorize_forward_access(&s.db, &claims, *id).await?;
    }
    let nodes: Vec<(String,)> =
        sqlx::query_as("SELECT DISTINCT node_id FROM forward_ports WHERE forward_id = ANY($1)")
            .bind(&req.ids)
            .fetch_all(&s.db)
            .await?;
    let mut tx = s.db.begin().await?;
    sqlx::query("DELETE FROM forwards WHERE id = ANY($1)")
        .bind(&req.ids)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    for (nid,) in &nodes {
        s.registry.push_config(&s.db, nid).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn batch_pause_forwards(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<BatchReq>,
) -> ApiResult<StatusCode> {
    for id in &req.ids {
        authorize_forward_access(&s.db, &claims, *id).await?;
    }
    sqlx::query(
        "UPDATE forwards SET desired_enabled = FALSE, updated_at = now() WHERE id = ANY($1)",
    )
    .bind(&req.ids)
    .execute(&s.db)
    .await?;
    bump_and_push_forwards(&s, &req.ids).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn batch_resume_forwards(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<BatchReq>,
) -> ApiResult<StatusCode> {
    for id in &req.ids {
        authorize_forward_access(&s.db, &claims, *id).await?;
    }
    sqlx::query(
        "UPDATE forwards SET desired_enabled = TRUE, updated_at = now() WHERE id = ANY($1)",
    )
    .bind(&req.ids)
    .execute(&s.db)
    .await?;
    bump_and_push_forwards(&s, &req.ids).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn batch_redeploy_forwards(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<BatchReq>,
) -> ApiResult<StatusCode> {
    for id in &req.ids {
        authorize_forward_access(&s.db, &claims, *id).await?;
    }
    sqlx::query(
        "UPDATE forwards SET deploy_generation = deploy_generation + 1,
                              last_deploy_error = NULL,
                              updated_at = now()
          WHERE id = ANY($1)",
    )
    .bind(&req.ids)
    .execute(&s.db)
    .await?;
    bump_and_push_forwards(&s, &req.ids).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Bump tunnels_version + push for every node touched by `forward_ids`.
async fn bump_and_push_forwards(s: &AppState, forward_ids: &[i64]) {
    if forward_ids.is_empty() {
        return;
    }
    let nodes: Vec<(String,)> = match sqlx::query_as(
        "SELECT DISTINCT node_id FROM forward_ports WHERE forward_id = ANY($1)",
    )
    .bind(forward_ids)
    .fetch_all(&s.db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "bump_and_push_forwards: fetch nodes failed");
            return;
        }
    };
    for (nid,) in &nodes {
        s.registry.push_config(&s.db, nid).await;
    }
}

// ── 公开状态页 ─────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct PublicNodeStatus {
    id: String,
    hostname: String,
    version: String,
    online: bool,
    last_seen_at: Option<DateTime<Utc>>,
    cpu_pct: Option<f64>,
    mem_pct: Option<f64>,
    mem_used_bytes: u64,
    mem_total_bytes: u64,
    active_connections: Option<i64>,
    net_rx_bps: f64,
    net_tx_bps: f64,
    // 最近 90 个小时桶在线分钟数（0-60，null=注册前无数据）
    history: Vec<Option<i32>>,
    uptime_90h: Option<f64>,
    // 最近 2 小时逐分钟在线状态（null=注册前无数据）
    recent_minutes: Vec<Option<bool>>,
}

#[derive(Serialize)]
struct PublicStatusResp {
    nodes: Vec<PublicNodeStatus>,
    announcement_enabled: bool,
    announcement_title: String,
    announcement_content: String,
}

async fn build_public_status(s: &AppState) -> Result<PublicStatusResp, sqlx::Error> {
    let mut nodes: Vec<Node> = sqlx::query_as("SELECT * FROM nodes ORDER BY id")
        .fetch_all(&s.db)
        .await?;
    s.overlay_nodes(&mut nodes).await;

    let net_speeds = s.series.all_node_net_speeds().await;

    let cfg: Option<crate::models::SystemConfig> = sqlx::query_as(
        "SELECT announcement_enabled, announcement_title, announcement_content, updated_at,
                monitor_retention_days
           FROM system_config WHERE id = 1",
    )
    .fetch_optional(&s.db)
    .await?;

    use std::collections::HashMap;

    // 查询所有节点过去 90 小时的在线历史（每小时桶，返回在线分钟数）
    #[derive(sqlx::FromRow)]
    struct HourRow {
        node_id: String,
        online_minutes: Option<i32>,
    }
    let node_ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let hour_rows: Vec<HourRow> = if node_ids.is_empty() {
        vec![]
    } else {
        sqlx::query_as(
            "SELECT n.id AS node_id,
                    CASE
                        WHEN gs.bucket < date_trunc('hour',
                             COALESCE(n.enrolled_at, now()) - INTERVAL '1 hour')
                        THEN NULL
                        ELSE (
                            SELECT COUNT(DISTINCT date_trunc('minute', a.recorded_at))::int
                              FROM node_availability a
                             WHERE a.node_id = n.id
                               AND a.recorded_at >= gs.bucket
                               AND a.recorded_at <  gs.bucket + INTERVAL '1 hour'
                        )
                    END AS online_minutes
               FROM nodes n
               CROSS JOIN generate_series(
                   date_trunc('hour', now()) - INTERVAL '89 hours',
                   date_trunc('hour', now()),
                   INTERVAL '1 hour'
               ) AS gs(bucket)
              WHERE n.id = ANY($1)
              ORDER BY n.id, gs.bucket",
        )
        .bind(&node_ids)
        .fetch_all(&s.db)
        .await?
    };

    let mut history_map: HashMap<String, Vec<Option<i32>>> = HashMap::new();
    for row in hour_rows {
        history_map
            .entry(row.node_id)
            .or_default()
            .push(row.online_minutes);
    }

    // 查询所有节点最近 2 小时的逐分钟在线状态
    #[derive(sqlx::FromRow)]
    struct MinuteRow {
        node_id: String,
        has_heartbeat: Option<bool>,
    }
    let minute_rows: Vec<MinuteRow> = if node_ids.is_empty() {
        vec![]
    } else {
        sqlx::query_as(
            "SELECT n.id AS node_id,
                    CASE
                        WHEN gs.bucket < date_trunc('minute',
                             COALESCE(n.enrolled_at, now()) - INTERVAL '1 minute')
                        THEN NULL
                        ELSE EXISTS(
                            SELECT 1 FROM node_availability a
                             WHERE a.node_id = n.id
                               AND a.recorded_at = gs.bucket
                        )
                    END AS has_heartbeat
               FROM nodes n
               CROSS JOIN generate_series(
                   date_trunc('minute', now()) - INTERVAL '119 minutes',
                   date_trunc('minute', now()),
                   INTERVAL '1 minute'
               ) AS gs(bucket)
              WHERE n.id = ANY($1)
              ORDER BY n.id, gs.bucket",
        )
        .bind(&node_ids)
        .fetch_all(&s.db)
        .await?
    };

    let mut recent_map: HashMap<String, Vec<Option<bool>>> = HashMap::new();
    for row in minute_rows {
        recent_map
            .entry(row.node_id)
            .or_default()
            .push(row.has_heartbeat);
    }

    let now = Utc::now();
    let node_statuses = nodes
        .into_iter()
        .map(|n| {
            let online = n
                .last_seen_at
                .map(|t| (now - t).num_milliseconds() < 15_000)
                .unwrap_or(false);

            let cpu_pct = n
                .last_heartbeat
                .as_ref()
                .and_then(|h| h.get("cpu_pct"))
                .and_then(|v| v.as_f64());

            let mem_used = n
                .last_heartbeat
                .as_ref()
                .and_then(|h| h.get("mem_used_bytes"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let mem_total = n
                .last_heartbeat
                .as_ref()
                .and_then(|h| h.get("mem_total_bytes"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let mem_pct = (mem_total > 0.0).then(|| mem_used / mem_total * 100.0);
            let mem_used_bytes = mem_used as u64;
            let mem_total_bytes = mem_total as u64;

            let active_connections = n
                .last_heartbeat
                .as_ref()
                .and_then(|h| h.get("active_connections"))
                .and_then(|v| v.as_i64());

            let (net_rx_bps, net_tx_bps) = net_speeds.get(&n.id).copied().unwrap_or_default();

            let history = history_map.remove(&n.id).unwrap_or_default();
            let known_minutes: Vec<i32> = history.iter().filter_map(|&m| m).collect();
            let uptime_90h = if known_minutes.is_empty() {
                None
            } else {
                let up: i32 = known_minutes.iter().sum();
                let total = known_minutes.len() as f64 * 60.0;
                Some((up as f64 / total * 100.0 * 100.0).round() / 100.0)
            };
            let recent_minutes = recent_map.remove(&n.id).unwrap_or_default();

            PublicNodeStatus {
                id: n.id,
                hostname: n.hostname,
                version: n.version,
                online,
                last_seen_at: n.last_seen_at,
                cpu_pct,
                mem_pct,
                mem_used_bytes,
                mem_total_bytes,
                active_connections,
                net_rx_bps,
                net_tx_bps,
                history,
                uptime_90h,
                recent_minutes,
            }
        })
        .collect();

    let (ann_enabled, ann_title, ann_content) = cfg
        .map(|c| {
            (
                c.announcement_enabled,
                c.announcement_title,
                c.announcement_content,
            )
        })
        .unwrap_or_default();

    Ok(PublicStatusResp {
        nodes: node_statuses,
        announcement_enabled: ann_enabled,
        announcement_title: ann_title,
        announcement_content: ann_content,
    })
}

async fn public_status(State(s): State<AppState>) -> ApiResult<Json<PublicStatusResp>> {
    build_public_status(&s)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

async fn forward_stats_stream(
    State(s): State<AppState>,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    use tokio_stream::wrappers::BroadcastStream;

    let rx = s.stats_tx.subscribe();
    let stream = BroadcastStream::new(rx).map(|res: Result<crate::state::ForwardStatEvent, _>| {
        Ok::<_, Infallible>(
            match res.ok().and_then(|e| serde_json::to_string(&e).ok()) {
                Some(j) => Event::default().data(j),
                None => Event::default().comment("lag"),
            },
        )
    });

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

// ── 连接日志 ──────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ConnectionLogItem {
    #[serde(with = "crate::snowflake::as_str")]
    id: i64,
    node_id: String,
    conn_id: String,
    client_ip: String,
    connected_at: DateTime<Utc>,
    disconnected_at: Option<DateTime<Utc>>,
    bytes_in: i64,
    bytes_out: i64,
}

#[derive(Deserialize)]
struct ConnectionsQuery {
    /// 页码，从 1 开始
    #[serde(default = "conn_default_page")]
    page: i64,
    #[serde(default = "conn_default_limit")]
    limit: i64,
}
fn conn_default_page() -> i64 {
    1
}
fn conn_default_limit() -> i64 {
    50
}

async fn forward_connections_handler(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<ConnectionsQuery>,
) -> ApiResult<Json<Vec<ConnectionLogItem>>> {
    let limit = q.limit.clamp(1, 200);
    let offset = (q.page.max(1) - 1) * limit;

    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT id, node_id, conn_id, client_ip, connected_at, disconnected_at, bytes_in, bytes_out
           FROM connection_logs
          WHERE forward_id = $1
          ORDER BY connected_at DESC
          LIMIT $2 OFFSET $3",
    )
    .bind(id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&s.db)
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let items = rows
        .iter()
        .map(|r| ConnectionLogItem {
            id: r.get("id"),
            node_id: r.get("node_id"),
            conn_id: r.get("conn_id"),
            client_ip: r.get("client_ip"),
            connected_at: r.get("connected_at"),
            disconnected_at: r.get("disconnected_at"),
            bytes_in: r.get("bytes_in"),
            bytes_out: r.get("bytes_out"),
        })
        .collect();

    Ok(Json(items))
}

// ── 转发历史流量统计 ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct StatsHistoryQuery {
    /// "1h"（默认）| "24h" | "7d"
    period: Option<String>,
}

#[derive(Debug, Serialize)]
struct StatsBucket {
    ts: DateTime<Utc>,
    bytes_in: i64,
    bytes_out: i64,
    peak_conns: i32,
}

async fn forward_stats_history(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<StatsHistoryQuery>,
) -> ApiResult<Json<Vec<StatsBucket>>> {
    // bucket_secs: 时间桶粒度（秒），用纯 PG 写法兼容原生 PG 和 TimescaleDB
    let (bucket_secs, since) = match q.period.as_deref() {
        Some("7d") => (3600_f64, "7 days"),
        Some("24h") => (300_f64, "24 hours"),
        Some("6h") => (120_f64, "6 hours"),
        _ => (60_f64, "1 hour"),
    };

    let rows = sqlx::query(
        "SELECT
             to_timestamp(floor(extract(epoch from ts) / $1) * $1) AS ts,
             SUM(bytes_in)::BIGINT                                  AS bytes_in,
             SUM(bytes_out)::BIGINT                                 AS bytes_out,
             MAX(peak_conns)::INT                                   AS peak_conns
         FROM forward_stats
         WHERE forward_id = $2
           AND ts >= now() - $3::interval
         GROUP BY 1
         ORDER BY 1",
    )
    .bind(bucket_secs)
    .bind(id)
    .bind(since)
    .fetch_all(&s.db)
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    use sqlx::Row;
    let buckets: Vec<StatsBucket> = rows
        .iter()
        .map(|r| StatsBucket {
            ts: r.get("ts"),
            bytes_in: r.get("bytes_in"),
            bytes_out: r.get("bytes_out"),
            peak_conns: r.get("peak_conns"),
        })
        .collect();

    Ok(Json(buckets))
}

async fn global_traffic_stats_handler(
    State(s): State<AppState>,
    Query(q): Query<StatsHistoryQuery>,
) -> ApiResult<Json<Vec<StatsBucket>>> {
    let (bucket_secs, since) = match q.period.as_deref() {
        Some("7d") => (3600_f64, "7 days"),
        Some("24h") => (300_f64, "24 hours"),
        Some("6h") => (120_f64, "6 hours"),
        _ => (60_f64, "1 hour"),
    };

    let rows = sqlx::query(
        "SELECT
             to_timestamp(floor(extract(epoch from ts) / $1) * $1) AS ts,
             SUM(bytes_in)::BIGINT                                  AS bytes_in,
             SUM(bytes_out)::BIGINT                                 AS bytes_out,
             0::INT                                                 AS peak_conns
         FROM node_stats
         WHERE ts >= now() - $2::interval
         GROUP BY 1
         ORDER BY 1",
    )
    .bind(bucket_secs)
    .bind(since)
    .fetch_all(&s.db)
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    use sqlx::Row;
    let buckets: Vec<StatsBucket> = rows
        .iter()
        .map(|r| StatsBucket {
            ts: r.get("ts"),
            bytes_in: r.get("bytes_in"),
            bytes_out: r.get("bytes_out"),
            peak_conns: r.get("peak_conns"),
        })
        .collect();

    Ok(Json(buckets))
}

// ── 节点流量统计 ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct NodeTrafficItem {
    node_id: String,
    hostname: String,
    bytes_in: i64,
    bytes_out: i64,
}

async fn node_traffic_stats_handler(
    State(s): State<AppState>,
    Query(q): Query<StatsHistoryQuery>,
) -> ApiResult<Json<Vec<NodeTrafficItem>>> {
    let secs: i64 = match q.period.as_deref() {
        Some("7d") => 7 * 24 * 3600,
        Some("24h") => 24 * 3600,
        Some("6h") => 6 * 3600,
        _ => 3600,
    };
    let since = chrono::Utc::now() - chrono::Duration::seconds(secs);

    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT ns.node_id,
                COALESCE(n.hostname, ns.node_id)      AS hostname,
                COALESCE(SUM(ns.bytes_in),  0)::BIGINT AS bytes_in,
                COALESCE(SUM(ns.bytes_out), 0)::BIGINT AS bytes_out
           FROM node_stats ns
           LEFT JOIN nodes n ON n.id = ns.node_id
          WHERE ns.ts >= $1
          GROUP BY ns.node_id, n.hostname
          ORDER BY SUM(ns.bytes_in) + SUM(ns.bytes_out) DESC",
    )
    .bind(since)
    .fetch_all(&s.db)
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let items: Vec<NodeTrafficItem> = rows
        .iter()
        .map(|r| NodeTrafficItem {
            node_id: r.get("node_id"),
            hostname: r.get("hostname"),
            bytes_in: r.get("bytes_in"),
            bytes_out: r.get("bytes_out"),
        })
        .collect();

    Ok(Json(items))
}

// ── Upstream probe 延迟统计 ────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct UpstreamProbeStats {
    upstream_addr: String,
    sample_count: i64,
    avg_latency_us: Option<f64>,
    /// 标准差（抖动）
    jitter_us: Option<f64>,
    /// 丢包率 0.0–1.0
    loss_rate: f64,
    last_probed_at: Option<DateTime<Utc>>,
}

async fn forward_probe_stats(
    State(s): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<UpstreamProbeStats>>> {
    let rows = sqlx::query(
        "SELECT
             upstream_addr,
             COUNT(*)                                                             AS sample_count,
             AVG(latency_us)  FILTER (WHERE latency_us IS NOT NULL)::float8   AS avg_latency_us,
             STDDEV(latency_us) FILTER (WHERE latency_us IS NOT NULL)::float8  AS jitter_us,
             1.0 - COUNT(*) FILTER (WHERE latency_us IS NOT NULL)::float8
                   / NULLIF(COUNT(*), 0)                                        AS loss_rate,
             MAX(probed_at)                                                  AS last_probed_at
         FROM forward_probe_samples
         WHERE forward_id = $1
           AND probed_at > NOW() - INTERVAL '24 hours'
         GROUP BY upstream_addr
         ORDER BY upstream_addr",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    use sqlx::Row;
    let stats: Vec<UpstreamProbeStats> = rows
        .iter()
        .map(|r| UpstreamProbeStats {
            upstream_addr: r.get("upstream_addr"),
            sample_count: r.get("sample_count"),
            avg_latency_us: r.get("avg_latency_us"),
            jitter_us: r.get("jitter_us"),
            loss_rate: r.get::<Option<f64>, _>("loss_rate").unwrap_or(0.0),
            last_probed_at: r.get("last_probed_at"),
        })
        .collect();

    Ok(Json(stats))
}

#[derive(Debug, Serialize)]
struct ProbeSample {
    ts: DateTime<Utc>,
    upstream_addr: String,
    avg_latency_us: Option<f64>,
    loss_rate: f64,
}

async fn forward_probe_series(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<i64>,
    Query(q): Query<StatsHistoryQuery>,
) -> ApiResult<Json<Vec<ProbeSample>>> {
    authorize_forward_access(&s.db, &claims, id).await?;

    let (bucket_secs, since) = match q.period.as_deref() {
        Some("7d") => (3600_f64, "7 days"),
        Some("24h") => (300_f64, "24 hours"),
        _ => (300_f64, "1 hour"),
    };

    let rows = sqlx::query(
        "SELECT
             upstream_addr,
             to_timestamp(floor(extract(epoch from probed_at) / $1) * $1) AS ts,
             AVG(latency_us) FILTER (WHERE latency_us IS NOT NULL)::float8  AS avg_latency_us,
             COUNT(*) FILTER (WHERE latency_us IS NULL)::float8
                 / NULLIF(COUNT(*), 0)::float8                             AS loss_rate
         FROM forward_probe_samples
         WHERE forward_id = $2
           AND probed_at >= now() - $3::interval
         GROUP BY upstream_addr, ts
         ORDER BY upstream_addr, ts",
    )
    .bind(bucket_secs)
    .bind(id)
    .bind(since)
    .fetch_all(&s.db)
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    use sqlx::Row;
    let samples: Vec<ProbeSample> = rows
        .iter()
        .map(|r| ProbeSample {
            ts: r.get("ts"),
            upstream_addr: r.get("upstream_addr"),
            avg_latency_us: r.get("avg_latency_us"),
            loss_rate: r.get::<Option<f64>, _>("loss_rate").unwrap_or(0.0),
        })
        .collect();

    Ok(Json(samples))
}

async fn public_status_stream(
    State(s): State<AppState>,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let stream = IntervalStream::new(interval).then(move |_| {
        let s = s.clone();
        async move {
            let event = match build_public_status(&s).await {
                Ok(data) => serde_json::to_string(&data)
                    .map(|j| Event::default().data(j))
                    .unwrap_or_else(|_| Event::default().comment("serialize error")),
                Err(_) => Event::default().comment("db error"),
            };
            Ok::<_, Infallible>(event)
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(30)))
}

// ── 系统全局配置 ───────────────────────────────────────────────────────────────

async fn get_config(State(s): State<AppState>) -> ApiResult<Json<crate::models::SystemConfig>> {
    let cfg: crate::models::SystemConfig = sqlx::query_as(
        "SELECT announcement_enabled, announcement_title, announcement_content, updated_at,
                monitor_retention_days
           FROM system_config WHERE id = 1",
    )
    .fetch_one(&s.db)
    .await?;
    Ok(Json(cfg))
}

#[derive(Deserialize)]
pub struct UpdateConfigReq {
    pub announcement_enabled: Option<bool>,
    pub announcement_title: Option<String>,
    pub announcement_content: Option<String>,
    pub monitor_retention_days: Option<i32>,
}

async fn update_config(
    State(s): State<AppState>,
    Json(req): Json<UpdateConfigReq>,
) -> ApiResult<Json<crate::models::SystemConfig>> {
    if let Some(days) = req.monitor_retention_days {
        if !(1..=3650).contains(&days) {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "monitor_retention_days 须在 1–3650 之间",
            ));
        }
    }
    let cfg: crate::models::SystemConfig = sqlx::query_as(
        "UPDATE system_config SET
            announcement_enabled   = COALESCE($1, announcement_enabled),
            announcement_title     = COALESCE($2, announcement_title),
            announcement_content   = COALESCE($3, announcement_content),
            monitor_retention_days = COALESCE($4, monitor_retention_days),
            updated_at             = now()
          WHERE id = 1
          RETURNING announcement_enabled, announcement_title, announcement_content, updated_at,
                    monitor_retention_days",
    )
    .bind(req.announcement_enabled)
    .bind(req.announcement_title)
    .bind(req.announcement_content)
    .bind(req.monitor_retention_days)
    .fetch_one(&s.db)
    .await?;
    Ok(Json(cfg))
}

// ── 用户组 ─────────────────────────────────────────────────────────────────────

async fn list_user_groups(State(s): State<AppState>) -> ApiResult<Json<Vec<UserGroupView>>> {
    type Row = (
        i64,
        String,
        String,
        i64,
        i64,
        i32,
        DateTime<Utc>,
        DateTime<Utc>,
        i64,
        i64,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT g.id, g.name, g.remark, g.flow_limit_bytes, g.speed_limit_kbps,
                g.forward_limit,
                g.created_at, g.updated_at,
                COUNT(DISTINCT gm.user_id)::BIGINT,
                COUNT(DISTINCT gt.id)::BIGINT
           FROM user_groups g
           LEFT JOIN group_members gm ON gm.group_id = g.id
           LEFT JOIN group_tunnels gt ON gt.group_id = g.id
          GROUP BY g.id
          ORDER BY g.created_at",
    )
    .fetch_all(&s.db)
    .await?;
    let out = rows
        .into_iter()
        .map(
            |(
                id,
                name,
                remark,
                flow_limit_bytes,
                speed_limit_kbps,
                forward_limit,
                created_at,
                updated_at,
                mc,
                tc,
            )| UserGroupView {
                group: UserGroup {
                    id,
                    name,
                    remark,
                    flow_limit_bytes,
                    speed_limit_kbps,
                    forward_limit,
                    created_at,
                    updated_at,
                },
                member_count: mc,
                tunnel_count: tc,
            },
        )
        .collect();
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct CreateUserGroupReq {
    pub name: String,
    #[serde(default)]
    pub remark: Option<String>,
    #[serde(default)]
    pub flow_limit_gb: Option<f64>,
    #[serde(default)]
    pub speed_limit_kbps: Option<i64>,
}

async fn create_user_group(
    State(s): State<AppState>,
    Json(req): Json<CreateUserGroupReq>,
) -> ApiResult<Json<UserGroup>> {
    if req.name.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "组名不能为空",
        ));
    }
    let flow_bytes = req
        .flow_limit_gb
        .map(|gb| (gb * 1_073_741_824.0) as i64)
        .unwrap_or(0);
    let row: UserGroup = sqlx::query_as(
        "INSERT INTO user_groups (id, name, remark, flow_limit_bytes, speed_limit_kbps)
         VALUES ($1, $2, COALESCE($3,''), $4, $5)
         RETURNING *",
    )
    .bind(crate::snowflake::next_id())
    .bind(req.name.trim())
    .bind(req.remark.as_deref())
    .bind(flow_bytes)
    .bind(req.speed_limit_kbps.unwrap_or(0))
    .fetch_one(&s.db)
    .await
    .map_err(map_db_err)?;
    Ok(Json(row))
}

async fn get_user_group(
    State(s): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<UserGroupView>> {
    type Row = (
        i64,
        String,
        String,
        i64,
        i64,
        i32,
        DateTime<Utc>,
        DateTime<Utc>,
        i64,
        i64,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT g.id, g.name, g.remark, g.flow_limit_bytes, g.speed_limit_kbps,
                g.forward_limit,
                g.created_at, g.updated_at,
                COUNT(DISTINCT gm.user_id)::BIGINT,
                COUNT(DISTINCT gt.id)::BIGINT
           FROM user_groups g
           LEFT JOIN group_members gm ON gm.group_id = g.id
           LEFT JOIN group_tunnels gt ON gt.group_id = g.id
          WHERE g.id = $1
          GROUP BY g.id",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await?;
    let (
        gid,
        name,
        remark,
        flow_limit_bytes,
        speed_limit_kbps,
        forward_limit,
        created_at,
        updated_at,
        mc,
        tc,
    ) = row.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "用户组不存在"))?;
    Ok(Json(UserGroupView {
        group: UserGroup {
            id: gid,
            name,
            remark,
            flow_limit_bytes,
            speed_limit_kbps,
            forward_limit,
            created_at,
            updated_at,
        },
        member_count: mc,
        tunnel_count: tc,
    }))
}

#[derive(Deserialize)]
pub struct UpdateUserGroupReq {
    pub name: Option<String>,
    pub remark: Option<String>,
    #[serde(default)]
    pub flow_limit_gb: Option<f64>,
    #[serde(default)]
    pub speed_limit_kbps: Option<i64>,
    #[serde(default)]
    pub forward_limit: Option<i32>,
}

async fn update_user_group(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateUserGroupReq>,
) -> ApiResult<Json<UserGroup>> {
    if req.name.is_none()
        && req.remark.is_none()
        && req.flow_limit_gb.is_none()
        && req.speed_limit_kbps.is_none()
        && req.forward_limit.is_none()
    {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "至少需要提供一个字段",
        ));
    }
    let flow_bytes = req.flow_limit_gb.map(|gb| (gb * 1_073_741_824.0) as i64);
    let updated: Option<UserGroup> = sqlx::query_as(
        "UPDATE user_groups SET
             name              = COALESCE($1, name),
             remark            = COALESCE($2, remark),
             flow_limit_bytes  = COALESCE($3, flow_limit_bytes),
             speed_limit_kbps  = COALESCE($4, speed_limit_kbps),
             forward_limit      = COALESCE($5, forward_limit)
         WHERE id = $6
         RETURNING *",
    )
    .bind(req.name.as_deref())
    .bind(req.remark.as_deref())
    .bind(flow_bytes)
    .bind(req.speed_limit_kbps)
    .bind(req.forward_limit)
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(map_db_err)?;
    updated
        .map(Json)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "用户组不存在"))
}

async fn delete_user_group(
    State(s): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let res = sqlx::query("DELETE FROM user_groups WHERE id = $1")
        .bind(id)
        .execute(&s.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "用户组不存在"));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── 组成员 ─────────────────────────────────────────────────────────────────────

async fn list_group_members(
    State(s): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<GroupMemberView>>> {
    type Row = (i64, String, String, String, DateTime<Utc>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT u.id, u.username, u.role, u.status, gm.created_at
           FROM group_members gm
           JOIN users u ON u.id = gm.user_id
          WHERE gm.group_id = $1
          ORDER BY gm.created_at",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await?;
    let out = rows
        .into_iter()
        .map(|(uid, username, role, status, added_at)| GroupMemberView {
            user_id: uid,
            username,
            role,
            status,
            added_at,
        })
        .collect();
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct AddGroupMemberReq {
    #[serde(with = "crate::snowflake::as_str")]
    pub user_id: i64,
}

async fn add_group_member(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<AddGroupMemberReq>,
) -> ApiResult<StatusCode> {
    let g_exists: Option<(bool,)> = sqlx::query_as("SELECT true FROM user_groups WHERE id = $1")
        .bind(id)
        .fetch_optional(&s.db)
        .await?;
    if g_exists.is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "用户组不存在"));
    }
    let u_exists: Option<(bool,)> = sqlx::query_as("SELECT true FROM users WHERE id = $1")
        .bind(req.user_id)
        .fetch_optional(&s.db)
        .await?;
    if u_exists.is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "用户不存在"));
    }
    sqlx::query(
        "INSERT INTO group_members (group_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(req.user_id)
    .execute(&s.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_group_member(
    State(s): State<AppState>,
    Path((id, user_id)): Path<(i64, i64)>,
) -> ApiResult<StatusCode> {
    let res = sqlx::query("DELETE FROM group_members WHERE group_id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(&s.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "成员不存在"));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── 组隧道分配 ─────────────────────────────────────────────────────────────────

async fn list_group_tunnels(
    State(s): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<GroupTunnelView>>> {
    type Row = (
        i64,
        i64,
        i64,
        bool,
        DateTime<Utc>,
        DateTime<Utc>,
        String,
        Vec<String>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT gt.id, gt.group_id, gt.tunnel_id, gt.enabled, gt.created_at, gt.updated_at,
                t.name, t.protocols
           FROM group_tunnels gt
           JOIN tunnels t ON t.id = gt.tunnel_id
          WHERE gt.group_id = $1
          ORDER BY gt.created_at",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await?;
    let out = rows
        .into_iter()
        .map(
            |(gid, group_id, tid, en, cre, upd, tname, proto)| GroupTunnelView {
                group_tunnel: GroupTunnel {
                    id: gid,
                    group_id,
                    tunnel_id: tid,
                    enabled: en,
                    created_at: cre,
                    updated_at: upd,
                },
                tunnel_name: tname,
                tunnel_protocols: proto,
            },
        )
        .collect();
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct CreateGroupTunnelReq {
    #[serde(with = "crate::snowflake::as_str")]
    pub tunnel_id: i64,
    #[serde(default)]
    pub enabled: Option<bool>,
}

async fn create_group_tunnel(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<CreateGroupTunnelReq>,
) -> ApiResult<Json<GroupTunnelView>> {
    let exists: Option<(bool,)> = sqlx::query_as("SELECT true FROM user_groups WHERE id = $1")
        .bind(id)
        .fetch_optional(&s.db)
        .await?;
    if exists.is_none() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "用户组不存在"));
    }
    type Row = (
        i64,
        i64,
        i64,
        bool,
        DateTime<Utc>,
        DateTime<Utc>,
        String,
        Vec<String>,
    );
    let row: Row = sqlx::query_as(
        "INSERT INTO group_tunnels (id, group_id, tunnel_id, enabled)
         VALUES ($1, $2, $3, COALESCE($4, true))
         RETURNING
             id, group_id, tunnel_id, enabled, created_at, updated_at,
             (SELECT name FROM tunnels WHERE id = tunnel_id),
             (SELECT protocols FROM tunnels WHERE id = tunnel_id)",
    )
    .bind(crate::snowflake::next_id())
    .bind(id)
    .bind(req.tunnel_id)
    .bind(req.enabled)
    .fetch_one(&s.db)
    .await
    .map_err(map_db_err)?;
    let (gid, group_id, tid, en, cre, upd, tname, proto) = row;
    Ok(Json(GroupTunnelView {
        group_tunnel: GroupTunnel {
            id: gid,
            group_id,
            tunnel_id: tid,
            enabled: en,
            created_at: cre,
            updated_at: upd,
        },
        tunnel_name: tname,
        tunnel_protocols: proto,
    }))
}

#[derive(Deserialize)]
pub struct UpdateGroupTunnelReq {
    #[serde(default)]
    pub enabled: Option<bool>,
}

async fn update_group_tunnel(
    State(s): State<AppState>,
    Path((_id, gt_id)): Path<(i64, i64)>,
    Json(req): Json<UpdateGroupTunnelReq>,
) -> ApiResult<Json<GroupTunnelView>> {
    type Row = (
        i64,
        i64,
        i64,
        bool,
        DateTime<Utc>,
        DateTime<Utc>,
        String,
        Vec<String>,
    );
    let row: Option<Row> = sqlx::query_as(
        "UPDATE group_tunnels SET
             enabled = COALESCE($1, enabled)
         WHERE id = $2
         RETURNING
             id, group_id, tunnel_id, enabled, created_at, updated_at,
             (SELECT name FROM tunnels WHERE id = tunnel_id),
             (SELECT protocols FROM tunnels WHERE id = tunnel_id)",
    )
    .bind(req.enabled)
    .bind(gt_id)
    .fetch_optional(&s.db)
    .await?;
    let (gid, group_id, tid, en, cre, upd, tname, proto) =
        row.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "组隧道分配不存在"))?;
    Ok(Json(GroupTunnelView {
        group_tunnel: GroupTunnel {
            id: gid,
            group_id,
            tunnel_id: tid,
            enabled: en,
            created_at: cre,
            updated_at: upd,
        },
        tunnel_name: tname,
        tunnel_protocols: proto,
    }))
}

async fn delete_group_tunnel(
    State(s): State<AppState>,
    Path((_id, gt_id)): Path<(i64, i64)>,
) -> ApiResult<StatusCode> {
    let res = sqlx::query("DELETE FROM group_tunnels WHERE id = $1")
        .bind(gt_id)
        .execute(&s.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "组隧道分配不存在"));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── 同步组配置到成员 user_tunnels ──────────────────────────────────────────────

#[derive(Serialize)]
pub struct ApplyGroupResult {
    pub applied: i64,
    pub skipped: i64,
}

async fn apply_group_tunnels(
    State(s): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<ApplyGroupResult>> {
    // 读取套餐级配额（expires_at 已在 0012 迁移中从 group_tunnels 移除，套餐不再统一设过期）
    let quota: Option<(i64, i64)> =
        sqlx::query_as("SELECT flow_limit_bytes, speed_limit_kbps FROM user_groups WHERE id = $1")
            .bind(id)
            .fetch_optional(&s.db)
            .await?;
    let (flow_limit_bytes, speed_limit_kbps) =
        quota.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "用户组不存在"))?;

    let members: Vec<(i64,)> =
        sqlx::query_as("SELECT user_id FROM group_members WHERE group_id = $1")
            .bind(id)
            .fetch_all(&s.db)
            .await?;
    let group_tunnels: Vec<GroupTunnel> =
        sqlx::query_as("SELECT * FROM group_tunnels WHERE group_id = $1")
            .bind(id)
            .fetch_all(&s.db)
            .await?;
    let mut applied: i64 = 0;
    for (user_id,) in &members {
        for gt in &group_tunnels {
            sqlx::query(
                "INSERT INTO user_tunnels
                     (id, user_id, tunnel_id, flow_limit_bytes, speed_limit_kbps, enabled)
                 VALUES ($6, $1, $2, $3, $4, $5)
                 ON CONFLICT (user_id, tunnel_id) DO UPDATE SET
                     flow_limit_bytes  = EXCLUDED.flow_limit_bytes,
                     speed_limit_kbps  = EXCLUDED.speed_limit_kbps,
                     enabled           = EXCLUDED.enabled",
            )
            .bind(user_id)
            .bind(gt.tunnel_id)
            .bind(flow_limit_bytes)
            .bind(speed_limit_kbps)
            .bind(gt.enabled)
            .bind(crate::snowflake::next_id())
            .execute(&s.db)
            .await?;
            applied += 1;
        }
    }
    Ok(Json(ApplyGroupResult {
        applied,
        skipped: 0,
    }))
}

// ============================================================================
// Upgrade endpoints (节点远程升级 — plan v3)
// ============================================================================

#[derive(Serialize)]
struct VersionResp {
    master_version: String,
    channel: String,
    latest_stable: Option<crate::upgrade::ResolvedRelease>,
    latest_rc: Option<crate::upgrade::ResolvedRelease>,
}

async fn get_system_version(State(s): State<AppState>) -> ApiResult<Json<VersionResp>> {
    let channel = read_channel(&s.db)
        .await
        .unwrap_or_else(|_| "stable".to_string());
    let stable = s.upgrade_resolver.latest_stable().await.ok();
    let rc = s.upgrade_resolver.latest_rc().await.ok();
    Ok(Json(VersionResp {
        master_version: env!("CARGO_PKG_VERSION").to_string(),
        channel,
        latest_stable: stable,
        latest_rc: rc,
    }))
}

#[derive(Serialize)]
struct ChannelResp {
    channel: String,
}

#[derive(Deserialize)]
struct ChannelReq {
    channel: String,
}

async fn get_upgrade_channel(State(s): State<AppState>) -> ApiResult<Json<ChannelResp>> {
    let channel = read_channel(&s.db).await?;
    Ok(Json(ChannelResp { channel }))
}

async fn put_upgrade_channel(
    State(s): State<AppState>,
    Json(req): Json<ChannelReq>,
) -> ApiResult<Json<ChannelResp>> {
    if req.channel != "stable" && req.channel != "rc" {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "channel 必须是 stable 或 rc",
        ));
    }
    sqlx::query(
        "INSERT INTO app_settings (key, value) VALUES ('upgrade_channel', $1)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(&req.channel)
    .execute(&s.db)
    .await?;
    Ok(Json(ChannelResp {
        channel: req.channel,
    }))
}

async fn read_channel(db: &sqlx::PgPool) -> Result<String, sqlx::Error> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM app_settings WHERE key = 'upgrade_channel'")
            .fetch_optional(db)
            .await?;
    Ok(row.map(|r| r.0).unwrap_or_else(|| "stable".to_string()))
}

#[derive(Deserialize)]
struct SelfUpgradeReq {
    target: String, // "stable" | "rc" | "vX.Y.Z"
}

#[derive(Serialize)]
struct SelfUpgradeResp {
    tag: String,
}

async fn upgrade_self_handler(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<SelfUpgradeReq>,
) -> ApiResult<impl IntoResponse> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static IN_PROGRESS: AtomicBool = AtomicBool::new(false);

    require_admin(&claims)?;

    if IN_PROGRESS
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "已有进行中的升级，请稍候",
        ));
    }

    let resolved = s.upgrade_resolver.resolve(&req.target).await.map_err(|e| {
        IN_PROGRESS.store(false, Ordering::SeqCst);
        ApiError::new(StatusCode::BAD_GATEWAY, format!("无法解析目标版本：{e}"))
    })?;

    let asset_url = match std::env::consts::ARCH {
        "x86_64" => resolved.master_linux_amd64_url.clone(),
        "aarch64" => resolved.master_linux_arm64_url.clone(),
        a => {
            IN_PROGRESS.store(false, Ordering::SeqCst);
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("不支持的架构：{a}"),
            ));
        }
    }
    .ok_or_else(|| {
        IN_PROGRESS.store(false, Ordering::SeqCst);
        ApiError::new(
            StatusCode::CONFLICT,
            "目标 release 缺少对应架构的 master 资产",
        )
    })?;

    let sha_url = resolved.sha256_url.clone().ok_or_else(|| {
        IN_PROGRESS.store(false, Ordering::SeqCst);
        ApiError::new(StatusCode::CONFLICT, "目标 release 缺少 SHA256SUMS 资产")
    })?;

    let tag = resolved.tag.clone();
    tokio::spawn(async move {
        match do_self_upgrade(&asset_url, &sha_url, &tag).await {
            Ok(()) => {
                tracing::info!(%tag, "self-upgrade: binary replaced, exiting for systemd restart");
                std::process::exit(0);
            }
            Err(e) => {
                tracing::error!(error = %e, "self-upgrade failed");
                IN_PROGRESS.store(false, Ordering::SeqCst);
            }
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(SelfUpgradeResp { tag: resolved.tag }),
    ))
}

async fn do_self_upgrade(asset_url: &str, sha_url: &str, tag: &str) -> anyhow::Result<()> {
    use anyhow::Context;
    use flate2::read::GzDecoder;
    use sha2::{Digest, Sha256};
    use std::io::Read as _;
    use std::time::Duration;

    const DEST: &str = "/var/lib/relay-master/relay-master";

    let basename = asset_url
        .rsplit('/')
        .next()
        .with_context(|| format!("bad asset URL: {asset_url}"))?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent(concat!("relay-master/", env!("CARGO_PKG_VERSION")))
        .build()?;

    tracing::info!(%asset_url, "self-upgrade: downloading tarball");
    let tar_data = client
        .get(asset_url)
        .send()
        .await
        .context("download tarball")?
        .error_for_status()
        .context("tarball response")?
        .bytes()
        .await
        .context("read tarball bytes")?;

    tracing::info!(%sha_url, "self-upgrade: downloading SHA256SUMS");
    let sums_data = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?
        .get(sha_url)
        .send()
        .await
        .context("download SHA256SUMS")?
        .error_for_status()
        .context("SHA256SUMS response")?
        .bytes()
        .await
        .context("read SHA256SUMS bytes")?;

    let sums_str = std::str::from_utf8(&sums_data).context("SHA256SUMS not UTF-8")?;
    let expected = sums_str
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let (hash, fname) = l.split_once("  ").or_else(|| l.split_once(' '))?;
            (fname.trim() == basename).then(|| hash.trim().to_string())
        })
        .next()
        .with_context(|| format!("{basename} not found in SHA256SUMS"))?;
    let actual = hex::encode(Sha256::digest(&tar_data));
    anyhow::ensure!(
        actual == expected,
        "checksum mismatch: expected {expected}, got {actual}"
    );

    tracing::info!(%tag, "self-upgrade: extracting relay-master from archive");
    let mut archive = tar::Archive::new(GzDecoder::new(tar_data.as_ref()));
    let mut bin_data: Option<Vec<u8>> = None;
    for entry in archive.entries().context("read tar entries")? {
        let mut entry = entry.context("tar entry")?;
        let path = entry.path().context("entry path")?;
        if path.file_name().and_then(|f| f.to_str()) == Some("relay-master") {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .context("read binary from tar")?;
            bin_data = Some(buf);
            break;
        }
    }
    let bin_data = bin_data.context("relay-master not found in archive")?;

    let tmp = format!("{DEST}.new");
    std::fs::write(&tmp, &bin_data).with_context(|| format!("write {tmp}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("chmod {tmp}"))?;
    }
    std::fs::rename(&tmp, DEST).with_context(|| format!("rename {tmp} -> {DEST}"))?;
    Ok(())
}

#[derive(Serialize)]
struct BrandingResp {
    brand_name: String,
}

#[derive(Deserialize)]
struct BrandingReq {
    brand_name: String,
}

async fn read_brand_name(db: &sqlx::PgPool) -> Result<String, sqlx::Error> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM app_settings WHERE key = 'brand_name'")
            .fetch_optional(db)
            .await?;
    Ok(row.map(|r| r.0).unwrap_or_else(|| "RELAY".to_string()))
}

async fn get_branding(State(s): State<AppState>) -> ApiResult<Json<BrandingResp>> {
    let brand_name = read_brand_name(&s.db)
        .await
        .unwrap_or_else(|_| "RELAY".to_string());
    Ok(Json(BrandingResp { brand_name }))
}

async fn put_branding(
    State(s): State<AppState>,
    Json(req): Json<BrandingReq>,
) -> ApiResult<Json<BrandingResp>> {
    let trimmed = req.brand_name.trim();
    if trimmed.is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "brand_name 不能为空",
        ));
    }
    if trimmed.chars().count() > 32 {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "brand_name 最长 32 个字符",
        ));
    }
    sqlx::query(
        "INSERT INTO app_settings (key, value) VALUES ('brand_name', $1)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(trimmed)
    .execute(&s.db)
    .await?;
    Ok(Json(BrandingResp {
        brand_name: trimmed.to_string(),
    }))
}

// ---------- R2 Backup Config ----------

use crate::backup::{read_r2_config, BackupJob, R2BackupConfig, R2_CONFIG_KEY};

#[derive(Serialize)]
struct R2BackupConfigResp {
    configured: bool,
    account_id: String,
    bucket_name: String,
    access_key_id: String,
    secret_access_key: String,
    path_prefix: String,
    schedule_hours: u32,
    keep_count: u32,
}

#[derive(Deserialize)]
struct R2BackupConfigReq {
    account_id: String,
    bucket_name: String,
    access_key_id: String,
    /// 留空表示不修改已保存的密钥
    secret_access_key: Option<String>,
    #[serde(default)]
    path_prefix: String,
    /// 0 = 禁用定时备份
    #[serde(default)]
    schedule_hours: u32,
    /// 0 = 不限制保留数量
    #[serde(default)]
    keep_count: u32,
}

fn to_r2_resp(c: &R2BackupConfig) -> R2BackupConfigResp {
    R2BackupConfigResp {
        configured: true,
        account_id: c.account_id.clone(),
        bucket_name: c.bucket_name.clone(),
        access_key_id: c.access_key_id.clone(),
        secret_access_key: c.secret_access_key.clone(),
        path_prefix: c.path_prefix.clone(),
        schedule_hours: c.schedule_hours,
        keep_count: c.keep_count,
    }
}

async fn get_r2_backup_config(State(s): State<AppState>) -> ApiResult<Json<R2BackupConfigResp>> {
    let resp = match read_r2_config(&s.db).await? {
        None => R2BackupConfigResp {
            configured: false,
            account_id: String::new(),
            bucket_name: String::new(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            path_prefix: String::new(),
            schedule_hours: 0,
            keep_count: 0,
        },
        Some(ref c) => to_r2_resp(c),
    };
    Ok(Json(resp))
}

async fn put_r2_backup_config(
    State(s): State<AppState>,
    Json(req): Json<R2BackupConfigReq>,
) -> ApiResult<Json<R2BackupConfigResp>> {
    if req.account_id.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "account_id 不能为空",
        ));
    }
    if req.bucket_name.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bucket_name 不能为空",
        ));
    }
    if req.access_key_id.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "access_key_id 不能为空",
        ));
    }

    let secret = match req.secret_access_key.as_deref() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => read_r2_config(&s.db)
            .await?
            .map(|c| c.secret_access_key)
            .unwrap_or_default(),
    };

    let cfg = R2BackupConfig {
        account_id: req.account_id.trim().to_string(),
        bucket_name: req.bucket_name.trim().to_string(),
        access_key_id: req.access_key_id.trim().to_string(),
        secret_access_key: secret,
        path_prefix: req.path_prefix.trim().to_string(),
        schedule_hours: req.schedule_hours,
        keep_count: req.keep_count,
    };
    let json_val = serde_json::to_string(&cfg)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    sqlx::query(
        "INSERT INTO app_settings (key, value) VALUES ($1, $2)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(R2_CONFIG_KEY)
    .bind(&json_val)
    .execute(&s.db)
    .await?;

    Ok(Json(to_r2_resp(&cfg)))
}

async fn trigger_backup(State(s): State<AppState>) -> ApiResult<StatusCode> {
    match crate::backup::read_r2_config(&s.db).await? {
        Some(c) if !c.account_id.is_empty() => {}
        _ => {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "R2 备份尚未配置，请先填写配置",
            ));
        }
    }
    s.backup_trigger.notify_one();
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
struct BackupJobsQuery {
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    20
}

async fn list_backup_jobs(
    State(s): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<BackupJobsQuery>,
) -> ApiResult<Json<Vec<BackupJob>>> {
    let limit = q.limit.clamp(1, 100);
    let jobs: Vec<BackupJob> = sqlx::query_as(
        "SELECT id, state, triggered_by, object_key, size_bytes, error, started_at, completed_at
           FROM backup_jobs
          ORDER BY started_at DESC
          LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&s.db)
    .await?;
    Ok(Json(jobs))
}

async fn list_backup_files(State(s): State<AppState>) -> ApiResult<Json<Vec<BackupJob>>> {
    let jobs: Vec<BackupJob> = sqlx::query_as(
        "SELECT id, state, triggered_by, object_key, size_bytes, error, started_at, completed_at
           FROM backup_jobs
          WHERE state = 'succeeded' AND object_key IS NOT NULL
          ORDER BY started_at DESC
          LIMIT 200",
    )
    .fetch_all(&s.db)
    .await?;
    Ok(Json(jobs))
}

#[derive(Deserialize)]
struct RestoreBackupReq {
    object_key: String,
}

async fn restore_backup_handler(
    State(s): State<AppState>,
    Json(req): Json<RestoreBackupReq>,
) -> ApiResult<StatusCode> {
    if req.object_key.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "object_key 不能为空",
        ));
    }
    let cfg = match crate::backup::read_r2_config(&s.db).await? {
        Some(c) if !c.account_id.is_empty() => c,
        _ => {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "R2 备份尚未配置",
            ));
        }
    };
    crate::backup::restore_from_r2(&s.db, &cfg, &req.object_key)
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct UpgradeNodeReq {
    /// "stable" / "rc" / "vX.Y.Z[-rc.*]"
    target: String,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct UpgradeJob {
    pub id: i64,
    pub node_id: String,
    pub from_version: Option<String>,
    pub target_tag: String,
    pub state: String,
    pub error: Option<String>,
    pub requested_by: i64,
    pub requested_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

async fn create_node_upgrade(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(node_id): Path<String>,
    Json(req): Json<UpgradeNodeReq>,
) -> ApiResult<Json<UpgradeJob>> {
    // 1) Node must exist + currently be online with upgrade_v1 capability.
    let from_version: Option<String> =
        sqlx::query_scalar("SELECT version FROM nodes WHERE id = $1")
            .bind(&node_id)
            .fetch_optional(&s.db)
            .await?
            .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "节点不存在"))?;

    let caps = {
        let map = s.node_runtime.read().await;
        map.get(&node_id)
            .map(|e| e.capabilities.clone())
            .unwrap_or_default()
    };
    if !caps.iter().any(|c| c == "upgrade_v1") {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "节点暂不支持远程升级（请先手动升级到 0.2.x 后再试）",
        ));
    }

    // 2) Refuse if there is already an in-flight job for this node.
    let active: Option<(i64,)> = sqlx::query_as(
        "SELECT id FROM upgrade_jobs
          WHERE node_id = $1
            AND state IN ('queued','dispatched','accepted')
          LIMIT 1",
    )
    .bind(&node_id)
    .fetch_optional(&s.db)
    .await?;
    if active.is_some() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "已有进行中的升级任务，请等其结束",
        ));
    }

    // 3) Resolve target.
    let resolved =
        s.upgrade_resolver.resolve(&req.target).await.map_err(|e| {
            ApiError::new(StatusCode::BAD_GATEWAY, format!("无法解析目标版本：{e}"))
        })?;

    // 3a) Reject if node is already on the target version. Otherwise the
    //     heartbeat-success scan would falsely succeed this job on the very
    //     next heartbeat without the updater ever running.
    if let Some(curr) = from_version.as_deref() {
        let target_norm = resolved.tag.strip_prefix('v').unwrap_or(&resolved.tag);
        if curr == target_norm {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                format!("节点已经是目标版本 {}", resolved.tag),
            ));
        }
    }

    let amd64_url = resolved
        .linux_amd64_url
        .as_deref()
        .ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "目标 release 缺少 linux/amd64 资产"))?;
    let arm64_url = resolved
        .linux_arm64_url
        .as_deref()
        .ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "目标 release 缺少 linux/arm64 资产"))?;
    let sha_url = resolved
        .sha256_url
        .as_deref()
        .ok_or_else(|| ApiError::new(StatusCode::CONFLICT, "目标 release 缺少 SHA256SUMS 资产"))?;

    // 4) Insert job row. The partial unique index on (node_id) WHERE state in
    //    (queued/dispatched/accepted) protects against a TOCTOU race with the
    //    "any active?" check above when two requests arrive concurrently.
    let job_id = crate::snowflake::next_id();
    let insert_res: Result<UpgradeJob, sqlx::Error> = sqlx::query_as(
        "INSERT INTO upgrade_jobs
            (id, node_id, from_version, target_tag, state, requested_by)
         VALUES ($1, $2, $3, $4, 'queued', $5)
         RETURNING *",
    )
    .bind(job_id)
    .bind(&node_id)
    .bind(from_version.as_deref())
    .bind(&resolved.tag)
    .bind(caller_id(&claims)?)
    .fetch_one(&s.db)
    .await;
    let job: UpgradeJob = match insert_res {
        Ok(j) => j,
        Err(sqlx::Error::Database(db_err))
            if db_err.constraint() == Some("upgrade_jobs_one_active_per_node") =>
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "已有进行中的升级任务，请等其结束",
            ));
        }
        Err(e) => return Err(e.into()),
    };

    // 5) Dispatch UPGRADE_AGENT command. Failures here transition job → failed.
    let mut args = std::collections::HashMap::new();
    args.insert("job_id".to_string(), job.id.to_string());
    args.insert("tag".to_string(), resolved.tag.clone());
    args.insert("asset_url_amd64".to_string(), amd64_url.to_string());
    args.insert("asset_url_arm64".to_string(), arm64_url.to_string());
    args.insert("sha256_url".to_string(), sha_url.to_string());
    let cmd = relay_proto::v1::Command {
        kind: relay_proto::v1::command::Kind::UpgradeAgent as i32,
        target_id: node_id.clone(),
        args,
    };

    match s.registry.send_command(&node_id, cmd).await {
        Ok(()) => {
            // Don't swallow errors here: if this UPDATE fails, the row stays
            // 'queued' and the heartbeat-success scan won't match it.
            sqlx::query("UPDATE upgrade_jobs SET state = 'dispatched' WHERE id = $1")
                .bind(job.id)
                .execute(&s.db)
                .await?;
            let mut out = job;
            out.state = "dispatched".to_string();
            Ok(Json(out))
        }
        Err(e) => {
            sqlx::query(
                "UPDATE upgrade_jobs
                    SET state = 'failed',
                        error = $2,
                        completed_at = now()
                  WHERE id = $1",
            )
            .bind(job.id)
            .bind(format!("dispatch failed: {e}"))
            .execute(&s.db)
            .await
            .ok();
            Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("派发失败：{e}"),
            ))
        }
    }
}

#[derive(Deserialize, Default)]
struct UpgradeJobsQuery {
    #[serde(default)]
    limit: Option<i64>,
}

async fn list_node_upgrade_jobs(
    State(s): State<AppState>,
    Path(node_id): Path<String>,
    Query(q): Query<UpgradeJobsQuery>,
) -> ApiResult<Json<Vec<UpgradeJob>>> {
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let rows: Vec<UpgradeJob> = sqlx::query_as(
        "SELECT * FROM upgrade_jobs
          WHERE node_id = $1
          ORDER BY requested_at DESC
          LIMIT $2",
    )
    .bind(&node_id)
    .bind(limit)
    .fetch_all(&s.db)
    .await?;
    Ok(Json(rows))
}

async fn get_upgrade_job(
    State(s): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<UpgradeJob>> {
    let row: Option<UpgradeJob> = sqlx::query_as("SELECT * FROM upgrade_jobs WHERE id = $1")
        .bind(id)
        .fetch_optional(&s.db)
        .await?;
    row.map(Json)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "升级任务不存在"))
}

/// 代理 GitHub raw 脚本，供国内用户通过 master 服务器下载安装脚本。
async fn proxy_script(Path(name): Path<String>) -> Response {
    const ALLOWED: &[&str] = &[
        "install.sh",
        "install-node.sh",
        "install-runner.sh",
        "uninstall.sh",
    ];
    if !ALLOWED.contains(&name.as_str()) {
        return (StatusCode::NOT_FOUND, "脚本不存在").into_response();
    }
    let url = format!(
        "https://raw.githubusercontent.com/0xUnixIO/relay/main/{}",
        name
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent(concat!("relay-master/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap();
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            let bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(_) => return (StatusCode::BAD_GATEWAY, "读取上游响应失败").into_response(),
            };
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                bytes,
            )
                .into_response()
        }
        Ok(resp) => {
            let status = resp.status();
            (StatusCode::BAD_GATEWAY, format!("上游返回 {status}")).into_response()
        }
        Err(_) => (StatusCode::BAD_GATEWAY, "无法连接 GitHub").into_response(),
    }
}
