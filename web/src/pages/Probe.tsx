import { useEffect, useState } from "react";
import useSWR from "swr";
import { Activity, CheckCircle, RefreshCw, XCircle } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { EmptyState } from "@/components/ui/empty-state";
import { ProbeTopology } from "@/components/ProbeTopology";
import { Api, type Forward, type ForwardProbeHop, type PublicStatus, type PublicNodeStatus } from "@/lib/api";
import { fmtBytes, timeAgo } from "@/lib/utils";
import { toast } from "sonner";

function fmtRate(bps: number): string {
  if (bps < 1024) return `${bps.toFixed(0)} B/s`;
  if (bps < 1024 * 1024) return `${(bps / 1024).toFixed(1)} KB/s`;
  return `${(bps / 1024 / 1024).toFixed(1)} MB/s`;
}

function MiniBar({ pct, color }: { pct: number; color: string }) {
  return (
    <div className="h-1.5 w-full rounded-full bg-black/10 dark:bg-white/10 overflow-hidden">
      <div
        className="h-full rounded-full transition-all duration-500"
        style={{ width: `${Math.min(100, pct)}%`, backgroundColor: color }}
      />
    </div>
  );
}

function hourBarColor(minutes: number | null): string {
  if (minutes === null) return "bg-muted-foreground/20";
  if (minutes === 0) return "bg-red-400/70";
  if (minutes < 50) return "bg-amber-400/80";
  return "bg-emerald-500/70";
}

function hourBarTitle(minutes: number | null): string {
  if (minutes === null) return "暂无数据";
  if (minutes === 0) return "离线";
  return `在线 ${minutes}/60 分钟`;
}

function HistoryBars({ history }: { history: (number | null)[] }) {
  if (history.length === 0) return null;
  return (
    <div className="flex gap-px h-4">
      {history.map((m, i) => (
        <div key={i} className={`flex-1 min-w-0 rounded-sm ${hourBarColor(m)}`} title={hourBarTitle(m)} />
      ))}
    </div>
  );
}

function NodeCard({ node }: { node: PublicNodeStatus }) {
  return (
    <div className="rounded-xl border bg-card p-4 space-y-3">
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2 min-w-0">
          <span className={`h-2 w-2 rounded-full shrink-0 ${node.online ? "bg-emerald-500" : "bg-muted-foreground/40"}`} />
          <span className="font-semibold text-sm truncate">{node.hostname || node.id}</span>
        </div>
        <div className="flex items-center gap-3 shrink-0 text-xs text-muted-foreground">
          {node.uptime_90h != null && <span className="tabular-nums">{node.uptime_90h.toFixed(1)}%</span>}
          {node.last_seen_at && <span>{timeAgo(node.last_seen_at)}</span>}
          <span className={`font-medium ${node.online ? "text-emerald-600 dark:text-emerald-400" : "text-muted-foreground"}`}>
            {node.online ? "在线" : "离线"}
          </span>
        </div>
      </div>

      {node.history.length > 0 && <HistoryBars history={node.history} />}

      {node.online && (node.cpu_pct != null || node.mem_pct != null) && (
        <div className="grid grid-cols-2 gap-x-4 gap-y-2 sm:grid-cols-3">
          {node.cpu_pct != null && (
            <div className="space-y-1">
              <div className="flex justify-between text-xs text-muted-foreground">
                <span>CPU</span>
                <span className="tabular-nums">{node.cpu_pct.toFixed(1)}%</span>
              </div>
              <MiniBar pct={node.cpu_pct} color={node.cpu_pct > 80 ? "#ef4444" : node.cpu_pct > 50 ? "#f59e0b" : "#38bdf8"} />
            </div>
          )}
          {node.mem_pct != null && (
            <div className="space-y-1">
              <div className="flex justify-between text-xs text-muted-foreground">
                <span>内存</span>
                <span className="tabular-nums">
                  {node.mem_pct.toFixed(1)}%
                  {node.mem_total_bytes > 0 && (
                    <span className="ml-1 opacity-60">{fmtBytes(node.mem_used_bytes)} / {fmtBytes(node.mem_total_bytes)}</span>
                  )}
                </span>
              </div>
              <MiniBar pct={node.mem_pct} color={node.mem_pct > 85 ? "#ef4444" : "#f59e0b"} />
            </div>
          )}
          {node.active_connections != null && (
            <div className="flex items-end gap-1 text-xs text-muted-foreground">
              <span className="font-semibold text-sm text-foreground tabular-nums">{node.active_connections}</span>
              活跃连接
            </div>
          )}
          {(node.net_rx_bps > 0 || node.net_tx_bps > 0) && (
            <div className="col-span-2 sm:col-span-3 flex gap-3 text-xs text-muted-foreground tabular-nums">
              <span>↓ {fmtRate(node.net_rx_bps)}</span>
              <span>↑ {fmtRate(node.net_tx_bps)}</span>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// ── 主页面 ────────────────────────────────────────────────────

export default function ProbePage() {
  const { data: forwards = [] } = useSWR("forwards", Api.listForwards);
  const [probing, setProbing] = useState<Record<string, boolean>>({});
  const [results, setResults] = useState<Record<string, ForwardProbeHop[]>>({});
  const [statusData, setStatusData] = useState<PublicStatus | null>(null);
  const [statusError, setStatusError] = useState(false);

  useEffect(() => {
    const es = new EventSource("/api/v1/status/stream");
    es.onmessage = (e) => {
      try { setStatusData(JSON.parse(e.data)); } catch {}
    };
    es.onerror = () => setStatusError(true);
    return () => es.close();
  }, []);

  const probe = async (f: Forward) => {
    setProbing((p) => ({ ...p, [f.id]: true }));
    try {
      const hops = await Api.probeForward(f.id);
      setResults((r) => ({ ...r, [f.id]: hops }));
    } catch (e: any) {
      toast.error(e?.message ?? "探测失败");
    } finally {
      setProbing((p) => ({ ...p, [f.id]: false }));
    }
  };

  const anyProbing = Object.values(probing).some(Boolean);
  const probeAll = () => { for (const f of forwards) probe(f); };

  const onlineCount = statusData?.nodes.filter((n) => n.online).length ?? 0;
  const totalCount = statusData?.nodes.length ?? 0;
  const allOk = totalCount > 0 && onlineCount === totalCount;

  return (
    <div className="space-y-8">
      {/* 节点状态 */}
      <div className="space-y-3">
        <h2 className="text-xs font-semibold uppercase tracking-widest text-muted-foreground/60">节点状态</h2>

        {statusError ? (
          <div className="rounded-lg border border-red-200 bg-red-50 p-3 flex items-center gap-2 dark:border-red-800/30 dark:bg-red-950/30">
            <XCircle className="h-4 w-4 text-red-500 shrink-0" />
            <span className="text-sm text-red-700 dark:text-red-300">无法获取状态数据</span>
          </div>
        ) : !statusData ? (
          <div className="rounded-lg border p-3 text-sm text-muted-foreground animate-pulse">加载中…</div>
        ) : (
          <>
            <div className={`rounded-lg border p-3 flex items-center gap-2 ${
              allOk
                ? "border-emerald-200 bg-emerald-50 dark:border-emerald-800/30 dark:bg-emerald-950/30"
                : "border-red-200 bg-red-50 dark:border-red-800/30 dark:bg-red-950/30"
            }`}>
              {allOk
                ? <CheckCircle className="h-4 w-4 text-emerald-500 shrink-0" />
                : <XCircle className="h-4 w-4 text-red-500 shrink-0" />}
              <span className={`font-medium text-sm ${allOk ? "text-emerald-900 dark:text-emerald-200" : "text-red-900 dark:text-red-200"}`}>
                {allOk ? "所有节点运行正常" : `${totalCount - onlineCount} 个节点离线`}
              </span>
              <span className="ml-auto text-xs text-muted-foreground tabular-nums">{onlineCount} / {totalCount} 在线</span>
            </div>
            {statusData.nodes.length > 0 && (
              <div className="space-y-3">
                {statusData.nodes.map((n) => <NodeCard key={n.id} node={n} />)}
              </div>
            )}
          </>
        )}
      </div>

      {/* 转发探测 */}
      <div className="space-y-3">
        <div className="flex items-center justify-between">
          <h2 className="text-xs font-semibold uppercase tracking-widest text-muted-foreground/60">转发探测</h2>
          <Button onClick={probeAll} disabled={forwards.length === 0 || anyProbing} variant="outline" size="sm">
            <RefreshCw className={`mr-1.5 h-3.5 w-3.5 ${anyProbing ? "animate-spin" : ""}`} />
            全部探测
          </Button>
        </div>

        {forwards.length === 0 ? (
          <EmptyState icon={Activity} title="暂无转发" description="创建转发后可在此发起延迟探测。" />
        ) : (
          <div className="space-y-3">
            {forwards.map((f) => {
              const result = results[f.id];
              const isProbing = probing[f.id];
              const hasError = result?.some((h) => !h.ok);

              return (
                <Card key={f.id}>
                  <CardContent className="p-4 space-y-3">
                    <div className="flex items-center justify-between gap-3">
                      <div className="flex items-center gap-2.5 min-w-0">
                        {result && (
                          <span className={`h-2 w-2 rounded-full shrink-0 ${hasError ? "bg-red-500" : "bg-emerald-500"}`} />
                        )}
                        <span className="font-medium truncate">{f.name}</span>
                        <span className="font-mono text-xs text-muted-foreground truncate hidden sm:block">
                          :{f.in_port}
                          {f.remote_addrs?.[0] && ` → ${f.remote_addrs[0]}`}
                          {f.remote_addrs?.length > 1 && ` +${f.remote_addrs.length - 1}`}
                        </span>
                      </div>
                      <Button size="sm" variant="outline" className="shrink-0" onClick={() => probe(f)} disabled={isProbing}>
                        <Activity className={`mr-1.5 h-3.5 w-3.5 ${isProbing ? "animate-pulse" : ""}`} />
                        {isProbing ? "探测中…" : result ? "重新探测" : "探测"}
                      </Button>
                    </div>

                    {result && (
                      <div className="space-y-2">
                        <ProbeTopology hops={result} />
                        {hasError && (
                          <div className="rounded-md border border-rose-200 bg-rose-50/60 p-2 text-xs text-rose-700 dark:border-rose-900/40 dark:bg-rose-950/30 dark:text-rose-400">
                            {result.filter((h) => !h.ok).map((h, i) => (
                              <div key={i} className="font-mono">{h.error}</div>
                            ))}
                          </div>
                        )}
                      </div>
                    )}
                  </CardContent>
                </Card>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
