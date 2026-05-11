import { timeAgo, fmtBytes } from "@/lib/utils";
import { Link } from "react-router-dom";
import { Server, Network, Zap, Activity } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Api, type NodeInfo, type ForwardStatBucket } from "@/lib/api";
import useSWR from "swr";
import { ProtocolSetBadge } from "@/components/ui/protocol-set-badge";
import { EmptyState } from "@/components/ui/empty-state";
import { useState } from "react";
import {
  AreaChart, Area,
  BarChart, Bar,
  XAxis, YAxis,
  Tooltip,
  ResponsiveContainer,
} from "recharts";

function isOnline(n: NodeInfo): boolean {
  if (!n.last_seen_at) return false;
  return Date.now() - new Date(n.last_seen_at).getTime() < 15_000;
}


function MiniBar({ value, max, color }: { value: number; max: number; color: string }) {
  const pct = max > 0 ? Math.min(100, (value / max) * 100) : 0;
  return (
    <div className="h-1 w-full rounded-full bg-border overflow-hidden">
      <div className="h-full rounded-full transition-all" style={{ width: `${pct}%`, backgroundColor: color }} />
    </div>
  );
}

type Period = "1h" | "6h" | "24h" | "7d";

function fmtTs(ts: string, period: Period): string {
  const d = new Date(ts);
  if (period === "7d") return `${d.getMonth() + 1}/${d.getDate()} ${d.getHours()}:00`;
  return `${d.getHours()}:${String(d.getMinutes()).padStart(2, "0")}`;
}

function FlowTooltip({ active, payload, label }: { active?: boolean; payload?: Array<{ value: number; name: string }>; label?: string }) {
  if (!active || !payload?.length) return null;
  return (
    <div className="rounded-md border bg-background px-2.5 py-1.5 text-xs shadow-sm space-y-0.5">
      {label && <div className="text-muted-foreground mb-1">{label}</div>}
      {payload.map((p) => (
        <div key={p.name} className="flex items-center gap-2">
          <span className="text-muted-foreground">{p.name}</span>
          <span className="font-medium tabular-nums">{fmtBytes(p.value)}</span>
        </div>
      ))}
    </div>
  );
}

export default function Dashboard() {
  const { data: nodes = [] } = useSWR("nodes", Api.listNodes);
  const { data: forwards = [] } = useSWR("forwards", Api.listForwards);

  // ---------- 流量趋势：DB 历史数据 ----------
  const [period, setPeriod] = useState<Period>("1h");
  const { data: trafficData = [] } = useSWR(
    ["globalTraffic", period],
    () => Api.getGlobalTrafficStats(period),
    { refreshInterval: 60_000 },
  );

  const chartData = trafficData.map((b: ForwardStatBucket) => ({
    ts: fmtTs(b.ts, period),
    bytes_in: b.bytes_in,
    bytes_out: b.bytes_out,
  }));

  // ---------- 其余统计 ----------
  const online = nodes.filter(isOnline);
  const offline = nodes.filter((n) => !isOnline(n));
  const enabled = forwards.filter((f) => f.effective_enabled);
  const totalConns = forwards.reduce((s, f) => s + (f.active_connections ?? 0), 0);

  // 各节点流量聚合（所有经过该节点的转发流量之和）
  const nodeTraffic = nodes
    .map((n) => {
      let bytes_in = 0, bytes_out = 0;
      for (const f of forwards) {
        if (f.ports.some((p) => p.node_id === n.id)) {
          bytes_in  += f.in_flow_bytes;
          bytes_out += f.out_flow_bytes;
        }
      }
      return { name: n.hostname || n.id.slice(0, 8), bytes_in, bytes_out };
    })
    .filter((n) => n.bytes_in > 0 || n.bytes_out > 0)
    .sort((a, b) => (b.bytes_in + b.bytes_out) - (a.bytes_in + a.bytes_out));

  // "active" = effective_enabled and entry node online.
  const active = enabled.filter((f) => {
    const entry = f.ports.find((p) => p.hop_index === 0)?.node_id;
    const node = nodes.find((n) => n.id === entry);
    return node && isOnline(node);
  });

  return (
    <div className="space-y-8">
      <div>
        <h1 className="text-2xl font-semibold">仪表</h1>
      </div>

      <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
        {[
          { icon: Server, label: "节点在线", value: `${online.length} / ${nodes.length}`, sub: nodes.length === 0 ? "暂无节点" : offline.length > 0 ? `${offline.length} 离线` : "全部在线" },
          { icon: Network, label: "转发启用", value: `${enabled.length} / ${forwards.length}`, sub: forwards.length === 0 ? "暂无转发" : `${forwards.length - enabled.length} 已停用` },
          { icon: Zap, label: "活跃连接", value: totalConns, sub: "所有节点合计" },
          { icon: Activity, label: "活跃转发", value: active.length, sub: "入口节点在线" },
        ].map(({ icon: Icon, label, value, sub }) => (
          <div key={label} className="rounded-lg border p-4 space-y-2">
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <span className="rounded-md bg-muted p-1.5 text-foreground/80">
                <Icon className="h-3.5 w-3.5" />
              </span>
              {label}
            </div>
            <div className="text-2xl font-semibold tabular-nums">{value}</div>
            <div className="text-sm text-muted-foreground">{sub}</div>
          </div>
        ))}
      </div>

      {/* 流量趋势图 */}
      <div className="rounded-lg border p-4 space-y-3">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-4">
            <h2 className="text-sm font-medium text-muted-foreground uppercase tracking-wide">
              流量趋势
            </h2>
            <div className="flex items-center gap-1">
              {(["1h", "6h", "24h", "7d"] as Period[]).map((p) => (
                <Button key={p} size="sm" variant={period === p ? "default" : "ghost"}
                  className="h-6 px-2 text-xs" onClick={() => setPeriod(p)}>
                  {p === "1h" ? "1 小时" : p === "6h" ? "6 小时" : p === "24h" ? "24 小时" : "7 天"}
                </Button>
              ))}
            </div>
          </div>
          <div className="flex items-center gap-4 text-xs text-muted-foreground">
            <span className="flex items-center gap-1.5">
              <span className="inline-block h-2 w-2 rounded-sm" style={{ backgroundColor: "hsl(var(--primary))" }} />
              ↓ 下载
            </span>
            <span className="flex items-center gap-1.5">
              <span className="inline-block h-2 w-2 rounded-sm" style={{ backgroundColor: "#10b981" }} />
              ↑ 上传
            </span>
          </div>
        </div>

        {chartData.length === 0 ? (
          <div className="flex h-40 items-center justify-center text-sm text-muted-foreground">
            暂无数据
          </div>
        ) : (
          <ResponsiveContainer width="100%" height={160}>
            <AreaChart data={chartData} margin={{ top: 4, right: 0, left: 0, bottom: 0 }}>
              <defs>
                <linearGradient id="gradIn" x1="0" y1="0" x2="0" y2="1">
                  <stop offset="5%" stopColor="hsl(var(--primary))" stopOpacity={0.25} />
                  <stop offset="95%" stopColor="hsl(var(--primary))" stopOpacity={0} />
                </linearGradient>
                <linearGradient id="gradOut" x1="0" y1="0" x2="0" y2="1">
                  <stop offset="5%" stopColor="#10b981" stopOpacity={0.25} />
                  <stop offset="95%" stopColor="#10b981" stopOpacity={0} />
                </linearGradient>
              </defs>
              <XAxis dataKey="ts" tick={{ fontSize: 10 }} tickLine={false} axisLine={false} interval="preserveStartEnd" />
              <YAxis
                width={68}
                tickLine={false}
                axisLine={false}
                tick={{ fontSize: 11 }}
                tickFormatter={(v: number) => fmtBytes(v)}
              />
              <Tooltip content={<FlowTooltip />} />
              <Area
                type="monotone"
                dataKey="bytes_in"
                name="下载"
                stroke="hsl(var(--primary))"
                strokeWidth={1.5}
                fill="url(#gradIn)"
                dot={false}
                isAnimationActive={false}
              />
              <Area
                type="monotone"
                dataKey="bytes_out"
                name="上传"
                stroke="#10b981"
                strokeWidth={1.5}
                fill="url(#gradOut)"
                dot={false}
                isAnimationActive={false}
              />
            </AreaChart>
          </ResponsiveContainer>
        )}
      </div>

      {/* 节点流量分布 */}
      {nodeTraffic.length > 0 && (
        <div className="rounded-lg border p-4 space-y-3">
          <div className="flex items-center justify-between">
            <h2 className="text-sm font-medium text-muted-foreground uppercase tracking-wide">节点流量分布</h2>
            <div className="flex items-center gap-4 text-xs text-muted-foreground">
              <span className="flex items-center gap-1.5">
                <span className="inline-block h-2 w-2 rounded-sm" style={{ backgroundColor: "hsl(var(--primary))" }} />
                ↓ 下载
              </span>
              <span className="flex items-center gap-1.5">
                <span className="inline-block h-2 w-2 rounded-sm" style={{ backgroundColor: "#10b981" }} />
                ↑ 上传
              </span>
            </div>
          </div>
          <ResponsiveContainer width="100%" height={Math.max(120, nodeTraffic.length * 40)}>
            <BarChart
              data={nodeTraffic}
              layout="vertical"
              margin={{ top: 0, right: 8, left: 0, bottom: 0 }}
              barCategoryGap="30%"
            >
              <XAxis type="number" tick={{ fontSize: 10 }} tickLine={false} axisLine={false}
                tickFormatter={(v: number) => fmtBytes(v)} width={64} />
              <YAxis type="category" dataKey="name" tick={{ fontSize: 11 }} tickLine={false}
                axisLine={false} width={80} />
              <Tooltip
                formatter={(v, name) => [fmtBytes(Number(v)), name === "bytes_in" ? "下载" : "上传"]}
                contentStyle={{ background: "hsl(var(--background))", border: "1px solid hsl(var(--border))", borderRadius: 8, fontSize: 12 }}
              />
              <Bar dataKey="bytes_in" name="下载" fill="hsl(var(--primary))" opacity={0.85} radius={[0, 3, 3, 0]} />
              <Bar dataKey="bytes_out" name="上传" fill="#10b981" opacity={0.85} radius={[0, 3, 3, 0]} />
            </BarChart>
          </ResponsiveContainer>
        </div>
      )}

      <div className="space-y-3">
        <h2 className="text-sm font-medium text-muted-foreground uppercase tracking-wide">节点状态</h2>
        {nodes.length === 0 ? (
          <EmptyState icon={Server} title="暂无节点" description="新增并启动节点 agent 后会自动出现在这里。" />
        ) : (
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {nodes.map((n) => {
              const on = isOnline(n);
              const hb = n.last_heartbeat;
              const cpu = hb?.cpu_pct ?? 0;
              const memUsed = hb?.mem_used_bytes ?? 0;
              const memTotal = hb?.mem_total_bytes ?? 0;
              const conns = hb?.active_connections ?? 0;
              const nodeForwards = forwards.filter((f) =>
                f.ports.some((p) => p.node_id === n.id),
              );
              return (
                <Link
                  key={n.id}
                  to={`/nodes/${n.id}`}
                  className="rounded-lg border p-4 space-y-3 hover:bg-accent transition-colors block"
                >
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-2 min-w-0">
                      <span className={`h-2 w-2 rounded-full shrink-0 ${on ? "bg-emerald-500" : "bg-border"}`} />
                      <span className="text-sm font-medium truncate">{n.hostname || n.id}</span>
                    </div>
                    <div className="flex items-center gap-2 shrink-0">
                      {nodeForwards.length > 0 && (
                        <span className="text-sm text-muted-foreground">{nodeForwards.length} 转发</span>
                      )}
                      <Badge variant={on ? "success" : "outline"}>{on ? "在线" : "离线"}</Badge>
                    </div>
                  </div>

                  {on && hb ? (
                    <div className="space-y-2">
                      <div className="space-y-1">
                        <div className="flex justify-between text-sm text-muted-foreground">
                          <span>CPU</span>
                          <span>{cpu.toFixed(1)}%</span>
                        </div>
                        <MiniBar value={cpu} max={100} color={cpu > 80 ? "#ef4444" : cpu > 50 ? "#f59e0b" : "#10b981"} />
                      </div>
                      {memTotal > 0 && (
                        <div className="space-y-1">
                          <div className="flex justify-between text-sm text-muted-foreground">
                            <span>内存</span>
                            <span>{fmtBytes(memUsed)} / {fmtBytes(memTotal)}</span>
                          </div>
                          <MiniBar value={memUsed} max={memTotal} color={memUsed / memTotal > 0.85 ? "#ef4444" : "#6366f1"} />
                        </div>
                      )}
                      {conns > 0 && (
                        <div className="text-sm text-muted-foreground">
                          {conns} 活跃连接
                        </div>
                      )}
                    </div>
                  ) : (
                    <div className="text-sm text-muted-foreground">
                      {n.last_seen_at
                        ? `最后上报 ${new Date(n.last_seen_at).toLocaleString()}（${timeAgo(n.last_seen_at)}）`
                        : "从未连接"}
                    </div>
                  )}
                </Link>
              );
            })}
          </div>
        )}
      </div>

      {active.length > 0 && (
        <div className="space-y-3">
          <h2 className="text-sm font-medium text-muted-foreground uppercase tracking-wide">活跃转发</h2>
          <div className="rounded-lg border divide-y">
            {active.map((f) => {
              return (
                <div key={f.id} className="flex items-center justify-between px-4 py-3 text-sm">
                  <div className="flex items-center gap-3 min-w-0">
                    <ProtocolSetBadge protocols={f.protocols} />
                    <span className="font-medium truncate">{f.name}</span>
                  </div>
                  <div className="flex items-center gap-4 shrink-0 ml-4">
                    {f.active_connections > 0 && (
                      <span className="text-sm text-emerald-500 tabular-nums">{f.active_connections} 连接</span>
                    )}
                    <span className="font-mono text-xs text-muted-foreground hidden sm:flex gap-1.5">
                      <span title="入站">↓{fmtBytes(f.in_flow_bytes)}</span>
                      <span title="出站">↑{fmtBytes(f.out_flow_bytes)}</span>
                    </span>
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
