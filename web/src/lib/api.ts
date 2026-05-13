import { getToken, clearAuth } from "./auth";

export class ApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

export async function api<T = any>(
  path: string,
  opts: RequestInit = {},
): Promise<T> {
  const headers = new Headers(opts.headers);
  if (!headers.has("content-type") && opts.body) {
    headers.set("content-type", "application/json");
  }
  const token = getToken();
  if (token) headers.set("authorization", `Bearer ${token}`);

  const res = await fetch(path, { ...opts, headers });
  if (res.status === 401) {
    clearAuth();
    if (!path.includes("/auth/")) {
      window.location.href = "/login";
    }
    throw new ApiError(401, "unauthorized");
  }
  if (!res.ok) {
    let msg = `HTTP ${res.status}`;
    try {
      const j = await res.json();
      if (j?.error) msg = j.error;
    } catch {}
    throw new ApiError(res.status, msg);
  }
  const ct = res.headers.get("content-type");
  if (!ct?.includes("application/json")) return undefined as T;
  return res.json();
}

// ---------- Auth ----------

export interface LoginResp {
  token: string;
  username: string;
  role: string;
}

// ---------- Users ----------

export interface User {
  id: string;
  username: string;
  role: string;
  status: string;
  expires_at: string | null;
  remark: string;
  created_at: string;
  updated_at: string;
  group_name?: string | null;
}

export interface UserPayload {
  username?: string;
  password?: string;
  role?: string;
  status?: string;
  expires_at?: string | null;
  remark?: string;
}

export interface MeResp extends User {
  forward_count: number;
  user_tunnel_count: number;
  group_name: string | null;
  flow_limit_bytes: number;
  speed_limit_kbps: number;
  forward_limit: number;
}

// ---------- Nodes ----------

export interface NodeInfo {
  id: string;
  hostname: string;
  version: string;
  protocol_version: number;
  tags: string[];
  server_ips: string[];
  port_range_start: number;
  port_range_end: number;
  traffic_ratio: number;
  tunnel_eligible: boolean;
  expires_at: string | null;
  monthly_price: number | null;
  website: string;
  enrolled_at: string | null;
  last_seen_at: string | null;
  last_heartbeat: any;
  cert_fingerprint: string | null;
  cert_serial: string | null;
  cert_not_after: string | null;
  created_at: string;
  updated_at: string;
  listen_stack?: string;
  capabilities?: string[];
}

export interface CreateNodeResp extends NodeInfo {
  enrollment_token: string;
}

export interface RotateTokenResp {
  id: string;
  enrollment_token: string;
}

export interface ServerInfo {
  public_host: string;
  public_hosts: string[];
  grpc_port: number;
  master_endpoint: string;
  enroll_endpoint: string;
  ca_cert_pem: string;
  ca_cert_b64: string;
  version: string;
}

// ---------- Tunnels ----------

export interface TunnelHopRef {
  hop_index: number;
  node_id: string;
}

export interface Tunnel {
  id: string;
  name: string;
  description: string;
  protocols: ("tcp" | "udp")[];
  ip_preference: string;
  in_ip: string;
  enabled: boolean;
  version: number;
  created_at: string;
  updated_at: string;
  hops: TunnelHopRef[];
  /** Layered DAG view: layers[i] is the list of node_ids at hop_index = i. */
  layers?: string[][];
  is_layered?: boolean;
  user_tunnel_count: number;
  forward_count: number;
  /** node_id → hostname */
  node_names?: Record<string, string>;
}

export interface CreateTunnelReq {
  name: string;
  description?: string;
  protocols?: ("tcp" | "udp")[];
  ip_preference?: string;
  in_ip?: string;
  /** Linear path. Use either node_ids OR layers, not both. */
  node_ids?: string[];
  /** Layered DAG path; layers[i] is the set of nodes at hop_index = i. */
  layers?: string[][];
  enabled?: boolean;
}

export interface UpdateTunnelReq {
  name?: string;
  description?: string;
  protocols?: ("tcp" | "udp")[];
  ip_preference?: string;
  in_ip?: string;
  enabled?: boolean;
  node_ids?: string[];
  layers?: string[][];
}

export interface TunnelProbeSegment {
  from_node: string;
  to: string;
  ok: boolean;
  latency_us: number;
  error: string;
}

export interface TunnelProbeResult {
  segments: TunnelProbeSegment[];
}

// ---------- Forwards ----------

export interface ForwardPort {
  forward_id: string;
  hop_index: number;
  node_id: string;
  protocol: string;
  listen_port: number;
}

export interface Forward {
  id: string;
  user_tunnel_id: string;
  user_id: string;
  username: string;
  tunnel_id: string;
  tunnel_name: string;
  protocols: ("tcp" | "udp")[];
  name: string;
  in_port: number;
  remote_addrs: string[];
  lb_strategy: string;
  max_connections: number;
  allow_cidrs: string[];
  deny_cidrs: string[];
  desired_enabled: boolean;
  effective_enabled: boolean;
  pause_reasons: string[];
  deploy_generation: number;
  in_flow_bytes: number;
  out_flow_bytes: number;
  last_deploy_error: string | null;
  ports: ForwardPort[];
  active_connections: number;
  created_at: string;
  updated_at: string;
  port_warnings?: string[];
  entry_addr: string | null;
  entry_addrs?: string[];
  /** lb_strategy='best' 时当前评分最优的上游地址 */
  best_exit_addr?: string | null;
  /** 当前活跃客户端 IP 列表（随 ForwardStats 快照更新） */
  active_client_ips?: string[];
  /** 所有跳节点均已回执当前配置版本 */
  synced?: boolean;
}

export interface CreateForwardReq {
  tunnel_id: string;
  name: string;
  in_port?: number;
  remote_addrs: string[];
  lb_strategy?: string;
  max_connections?: number;
  allow_cidrs?: string[];
  deny_cidrs?: string[];
}

export interface UpdateForwardReq {
  tunnel_id?: string;
  name?: string;
  in_port?: number;
  remote_addrs?: string[];
  lb_strategy?: string;
  max_connections?: number;
  allow_cidrs?: string[];
  deny_cidrs?: string[];
}

// ---------- Forward Probe ----------

export interface ForwardProbeHop {
  from_node: string;
  from_node_name: string;
  to_node?: string;
  to_node_name?: string;
  target: string;
  ok: boolean;
  latency_us: number;
  error: string;
}

// ---------- Probes / series ----------

export interface HeartbeatSample {
  ts_unix_ms: number;
  cpu_pct: number;
  mem_used_bytes: number;
  mem_total_bytes: number;
  active_connections: number;
}

export interface ForwardSample {
  ts_unix_ms: number;
  bytes_in: number;
  bytes_out: number;
  active_connections: number;
  total_connections: number;
}

export interface NodeSeries {
  heartbeats: HeartbeatSample[];
  /** Keyed by `${forward_id}:${hop_index}`. */
  tunnels: Record<string, ForwardSample[]>;
}

export interface ProbePortResponse {
  free: boolean;
  error: string;
}

export interface NodeSpeed {
  rx_bps: number;
  tx_bps: number;
}

export const Api = {
  // auth
  authStatus: () => api<{ bootstrapped: boolean }>("/api/v1/auth/status"),
  bootstrap: (username: string, password: string) =>
    api("/api/v1/auth/bootstrap", {
      method: "POST",
      body: JSON.stringify({ username, password }),
    }),
  login: (username: string, password: string) =>
    api<LoginResp>("/api/v1/auth/login", {
      method: "POST",
      body: JSON.stringify({ username, password }),
    }),
  changeOwnPassword: (body: { old_password: string; new_password: string }) =>
    api<void>("/api/v1/auth/me/password", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  getMe: () => api<MeResp>("/api/v1/auth/me"),

  // users
  listUsers: () => api<User[]>("/api/v1/users"),
  createUser: (body: UserPayload & { username: string; password: string; role: string }) =>
    api<User>("/api/v1/users", { method: "POST", body: JSON.stringify(body) }),
  updateUser: (id: string, body: UserPayload) =>
    api<User>(`/api/v1/users/${id}`, { method: "PUT", body: JSON.stringify(body) }),
  deleteUser: (id: string) =>
    api<void>(`/api/v1/users/${id}`, { method: "DELETE" }),

  // nodes
  listNodes: () => api<NodeInfo[]>("/api/v1/nodes"),
  getNode: (id: string) => api<NodeInfo>(`/api/v1/nodes/${id}`),
  getNodeSeries: (id: string) => api<NodeSeries>(`/api/v1/nodes/${id}/series`),
  getNodeSpeeds: () => api<Record<string, NodeSpeed>>("/api/v1/nodes/speeds"),
  createNode: (payload: {
    id?: string;
    hostname?: string;
    port_range_start?: number;
    port_range_end?: number;
  }) =>
    api<CreateNodeResp>("/api/v1/nodes", {
      method: "POST",
      body: JSON.stringify(payload),
    }),
  deleteNode: (id: string) =>
    api(`/api/v1/nodes/${id}`, { method: "DELETE" }),
  updateNode: (
    id: string,
    patch: {
      hostname?: string;
      tags?: string[];
      server_ips?: string[];
      port_range_start?: number;
      port_range_end?: number;
      traffic_ratio?: number;
    },
  ) =>
    api<NodeInfo>(`/api/v1/nodes/${id}`, {
      method: "PUT",
      body: JSON.stringify(patch),
    }),
  rotateNodeToken: (id: string) =>
    api<RotateTokenResp>(`/api/v1/nodes/${id}/rotate-token`, { method: "POST" }),
  createInstallBundle: (nodeId: string) =>
    api<{ token: string }>(`/api/v1/nodes/${nodeId}/setup`, { method: "POST" }),
  serverInfo: () => api<ServerInfo>("/api/v1/server-info"),
  probeNodePort: (
    nodeId: string,
    port: number,
    protocol: "tcp" | "udp" = "tcp",
  ) =>
    api<ProbePortResponse>(`/api/v1/nodes/${nodeId}/probe-port`, {
      method: "POST",
      body: JSON.stringify({ port, protocol }),
    }),

  // tunnels (admin)
  listTunnels: () => api<Tunnel[]>("/api/v1/tunnels"),
  getTunnel: (id: string) => api<Tunnel>(`/api/v1/tunnels/${id}`),
  createTunnel: (r: CreateTunnelReq) =>
    api<Tunnel>("/api/v1/tunnels", { method: "POST", body: JSON.stringify(r) }),
  updateTunnel: (id: string, r: UpdateTunnelReq) =>
    api<Tunnel>(`/api/v1/tunnels/${id}`, { method: "PUT", body: JSON.stringify(r) }),
  deleteTunnel: (id: string) =>
    api(`/api/v1/tunnels/${id}`, { method: "DELETE" }),
  probeTunnel: (id: string) =>
    api<TunnelProbeResult>(`/api/v1/tunnels/${id}/probe`, { method: "POST" }),

  // forwards
  listForwards: (params?: { user_id?: string; tunnel_id?: string }) => {
    const qs = new URLSearchParams();
    if (params?.user_id) qs.set("user_id", params.user_id);
    if (params?.tunnel_id) qs.set("tunnel_id", params.tunnel_id);
    const q = qs.toString();
    return api<Forward[]>(`/api/v1/forwards${q ? `?${q}` : ""}`);
  },
  getForward: (id: string) => api<Forward>(`/api/v1/forwards/${id}`),
  createForward: (r: CreateForwardReq) =>
    api<Forward>("/api/v1/forwards", {
      method: "POST",
      body: JSON.stringify(r),
    }),
  updateForward: (id: string, r: UpdateForwardReq) =>
    api<Forward>(`/api/v1/forwards/${id}`, {
      method: "PUT",
      body: JSON.stringify(r),
    }),
  deleteForward: (id: string) =>
    api(`/api/v1/forwards/${id}`, { method: "DELETE" }),
  pauseForward: (id: string) =>
    api<void>(`/api/v1/forwards/${id}/pause`, { method: "POST" }),
  resumeForward: (id: string) =>
    api<void>(`/api/v1/forwards/${id}/resume`, { method: "POST" }),
  redeployForward: (id: string) =>
    api<void>(`/api/v1/forwards/${id}/redeploy`, { method: "POST" }),
  probeForward: (id: string) =>
    api<ForwardProbeHop[]>(`/api/v1/forwards/${id}/probe`, { method: "POST" }),

  batchDeleteForwards: (ids: string[]) =>
    api<void>("/api/v1/forwards/batch/delete", {
      method: "POST",
      body: JSON.stringify({ ids }),
    }),
  batchPauseForwards: (ids: string[]) =>
    api<void>("/api/v1/forwards/batch/pause", {
      method: "POST",
      body: JSON.stringify({ ids }),
    }),
  batchResumeForwards: (ids: string[]) =>
    api<void>("/api/v1/forwards/batch/resume", {
      method: "POST",
      body: JSON.stringify({ ids }),
    }),
  batchRedeployForwards: (ids: string[]) =>
    api<void>("/api/v1/forwards/batch/redeploy", {
      method: "POST",
      body: JSON.stringify({ ids }),
    }),

  // ---------- System Config ----------
  getConfig: () => api<SystemConfig>("/api/v1/config"),
  updateConfig: (r: UpdateConfigReq) =>
    api<SystemConfig>("/api/v1/config", {
      method: "PUT",
      body: JSON.stringify(r),
    }),

  // ---------- Upgrade ----------
  getSystemVersion: () => api<SystemVersionResp>("/api/v1/system/version"),
  checkLatestVersion: () => api<LatestVersionResp>("/api/v1/system/version/latest"),
  selfUpgradeMaster: (target: string) =>
    api<{ tag: string }>("/api/v1/system/self-upgrade", {
      method: "POST",
      body: JSON.stringify({ target }),
    }),
  upgradeNode: (id: string, target: string) =>
    api<UpgradeJob>(`/api/v1/nodes/${id}/upgrade`, {
      method: "POST",
      body: JSON.stringify({ target }),
    }),
  listNodeUpgradeJobs: (id: string, limit = 10) =>
    api<UpgradeJob[]>(`/api/v1/nodes/${id}/upgrade/jobs?limit=${limit}`),
  getUpgradeJob: (id: string | number) => api<UpgradeJob>(`/api/v1/upgrade_jobs/${id}`),

  // ---------- Branding ----------
  getBranding: () => api<{ brand_name: string }>("/api/v1/system/branding"),
  setBranding: (brand_name: string) =>
    api<{ brand_name: string }>("/api/v1/system/branding", {
      method: "PUT",
      body: JSON.stringify({ brand_name }),
    }),

  // ---------- R2 Backup ----------
  getR2BackupConfig: () => api<R2BackupConfigResp>("/api/v1/system/backup/r2"),
  setR2BackupConfig: (req: R2BackupConfigReq) =>
    api<R2BackupConfigResp>("/api/v1/system/backup/r2", {
      method: "PUT",
      body: JSON.stringify(req),
    }),
  triggerBackup: () =>
    api<void>("/api/v1/system/backup/trigger", { method: "POST" }),
  listBackupJobs: (limit = 20) =>
    api<BackupJob[]>(`/api/v1/system/backup/jobs?limit=${limit}`),
  listBackupFiles: () =>
    api<R2BackupFile[]>("/api/v1/system/backup/files"),
  restoreBackup: (objectKey: string) =>
    api<void>("/api/v1/system/backup/restore", {
      method: "POST",
      body: JSON.stringify({ object_key: objectKey }),
    }),

  // ---------- User Groups ----------
  listUserGroups: () => api<UserGroup[]>("/api/v1/user-groups"),
  createUserGroup: (body: { name: string; remark?: string }) =>
    api<UserGroup>("/api/v1/user-groups", { method: "POST", body: JSON.stringify(body) }),
  updateUserGroup: (id: string, body: { name?: string; remark?: string; flow_limit_gb?: number; speed_limit_kbps?: number; forward_limit?: number }) =>
    api<UserGroup>(`/api/v1/user-groups/${id}`, { method: "PUT", body: JSON.stringify(body) }),
  deleteUserGroup: (id: string) =>
    api<void>(`/api/v1/user-groups/${id}`, { method: "DELETE" }),

  listGroupMembers: (id: string) =>
    api<GroupMember[]>(`/api/v1/user-groups/${id}/members`),
  addGroupMember: (id: string, user_id: string) =>
    api<void>(`/api/v1/user-groups/${id}/members`, {
      method: "POST",
      body: JSON.stringify({ user_id }),
    }),
  removeGroupMember: (id: string, user_id: string) =>
    api<void>(`/api/v1/user-groups/${id}/members/${user_id}`, { method: "DELETE" }),

  listGroupTunnels: (id: string) =>
    api<GroupTunnel[]>(`/api/v1/user-groups/${id}/tunnels`),
  createGroupTunnel: (id: string, r: CreateGroupTunnelReq) =>
    api<GroupTunnel>(`/api/v1/user-groups/${id}/tunnels`, {
      method: "POST",
      body: JSON.stringify(r),
    }),
  updateGroupTunnel: (id: string, gt_id: string, r: UpdateGroupTunnelReq) =>
    api<GroupTunnel>(`/api/v1/user-groups/${id}/tunnels/${gt_id}`, {
      method: "PUT",
      body: JSON.stringify(r),
    }),
  deleteGroupTunnel: (id: string, gt_id: string) =>
    api<void>(`/api/v1/user-groups/${id}/tunnels/${gt_id}`, { method: "DELETE" }),

  applyGroupTunnels: (id: string) =>
    api<ApplyGroupResult>(`/api/v1/user-groups/${id}/apply`, { method: "POST" }),

  getForwardStats: (id: string, period: "1h" | "6h" | "24h" | "7d" = "1h") =>
    api<ForwardStatBucket[]>(`/api/v1/forwards/${id}/stats?period=${period}`),

  getGlobalTrafficStats: (period: "1h" | "6h" | "24h" | "7d" = "1h") =>
    api<ForwardStatBucket[]>(`/api/v1/stats/traffic?period=${period}`),

  getNodeTrafficStats: (period: "1h" | "6h" | "24h" | "7d" = "1h") =>
    api<NodeTrafficItem[]>(`/api/v1/stats/nodes/traffic?period=${period}`),

  getForwardProbeStats: (id: string) =>
    api<UpstreamProbeStats[]>(`/api/v1/forwards/${id}/probe-stats`),

  getForwardProbeSeries: (id: string, period: "1h" | "24h" | "7d" = "24h") =>
    api<ProbeSample[]>(`/api/v1/forwards/${id}/probe-series?period=${period}`),

  listForwardConnections: (id: string, page = 1, limit = 50) =>
    api<ConnectionLog[]>(`/api/v1/forwards/${id}/connections?page=${page}&limit=${limit}`),
};

export interface SystemConfig {
  announcement_enabled: boolean;
  announcement_title: string;
  announcement_content: string;
  updated_at: string;
  monitor_retention_days: number;
}

export interface UpdateConfigReq {
  announcement_enabled?: boolean;
  announcement_title?: string;
  announcement_content?: string;
  monitor_retention_days?: number;
}

// ---------- Public Status ----------

export interface PublicNodeStatus {
  id: string;
  hostname: string;
  version: string;
  online: boolean;
  last_seen_at: string | null;
  cpu_pct: number | null;
  mem_pct: number | null;
  mem_used_bytes: number;
  mem_total_bytes: number;
  active_connections: number | null;
  net_rx_bps: number;
  net_tx_bps: number;
  history: (number | null)[];
  uptime_90h: number | null;
  recent_minutes: (boolean | null)[];
}

export interface PublicStatus {
  nodes: PublicNodeStatus[];
  announcement_enabled: boolean;
  announcement_title: string;
  announcement_content: string;
}


// ---------- Upgrade ----------

export interface ResolvedRelease {
  tag: string;
  prerelease: boolean;
  published_at: string | null;
  linux_amd64_url: string | null;
  linux_arm64_url: string | null;
  master_linux_amd64_url: string | null;
  master_linux_arm64_url: string | null;
  sha256_url: string | null;
}

export interface SystemVersionResp {
  master_version: string;
}

export interface LatestVersionResp {
  latest_stable: ResolvedRelease | null;
}

export interface R2BackupConfigResp {
  configured: boolean;
  account_id: string;
  bucket_name: string;
  access_key_id: string;
  secret_access_key: string;
  path_prefix: string;
  schedule_hours: number;
  keep_count: number;
}

export interface R2BackupConfigReq {
  account_id: string;
  bucket_name: string;
  access_key_id: string;
  /** 留空表示不修改已存储的密钥 */
  secret_access_key?: string;
  path_prefix?: string;
  schedule_hours?: number;
  keep_count?: number;
}

export type BackupJobState = "running" | "succeeded" | "failed";

export interface BackupJob {
  id: number;
  state: BackupJobState;
  triggered_by: "schedule" | "manual";
  object_key: string | null;
  size_bytes: number | null;
  error: string | null;
  started_at: string;
  completed_at: string | null;
}

export interface R2BackupFile {
  key: string;
  size: number;
  last_modified: string;
}

export type UpgradeJobState =
  | "queued"
  | "dispatched"
  | "accepted"
  | "succeeded"
  | "failed"
  | "timed_out";

export interface UpgradeJob {
  id: number;
  node_id: string;
  from_version: string | null;
  target_tag: string;
  state: UpgradeJobState;
  error: string | null;
  requested_by: number;
  requested_at: string;
  accepted_at: string | null;
  completed_at: string | null;
}

// ---------- User Groups ----------

export interface UserGroup {
  id: string;
  name: string;
  remark: string;
  member_count: number;
  tunnel_count: number;
  flow_limit_bytes: number;
  speed_limit_kbps: number;
  forward_limit: number;
  created_at: string;
  updated_at: string;
}

export interface GroupMember {
  user_id: string;
  username: string;
  role: string;
  status: string;
  added_at: string;
}

export interface GroupTunnel {
  id: string;
  group_id: string;
  tunnel_id: string;
  tunnel_name: string;
  tunnel_protocols: ("tcp" | "udp")[];
  enabled: boolean;
  created_at: string;
  updated_at: string;
}

export interface CreateGroupTunnelReq {
  tunnel_id: string;
  enabled?: boolean;
}

export interface UpdateGroupTunnelReq {
  flow_limit_gb?: number;
  speed_limit_kbps?: number;
  expires_at?: string | null;
  enabled?: boolean;
}

export interface ApplyGroupResult {
  applied: number;
  skipped: number;
}

export interface ForwardStatBucket {
  ts:         string;
  bytes_in:   number;
  bytes_out:  number;
  peak_conns: number;
}

export interface NodeTrafficItem {
  node_id:   string;
  hostname:  string;
  bytes_in:  number;
  bytes_out: number;
}

// ---------- Forward Probe Stats ----------

export interface ProbeSample {
  ts: string;
  upstream_addr: string;
  avg_latency_us: number | null;
  loss_rate: number;
}

// ---------- Connection Logs ----------

export interface ConnectionLog {
  id: string;
  node_id: string;
  conn_id: string;
  client_ip: string;
  connected_at: string;
  disconnected_at: string | null;
  bytes_in: number;
  bytes_out: number;
}

export interface UpstreamProbeStats {
  upstream_addr: string;
  sample_count: number;
  avg_latency_us: number | null;
  jitter_us: number | null;
  loss_rate: number;
  last_probed_at: string | null;
}
