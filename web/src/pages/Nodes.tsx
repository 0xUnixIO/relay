import { useState, useMemo } from "react";
import useSWR from "swr";
import { timeAgo } from "@/lib/utils";
import { useNavigate } from "react-router-dom";
import { Plus, Copy, Check, Search, Cpu, MemoryStick, Network, DatabaseZap, Loader2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Api, type NodeInfo, type NodeSpeed, ApiError } from "@/lib/api";
import { useNodeSpeeds } from "@/hooks/useNodeSpeeds";
import { useSetupLink } from "@/hooks/useSetupLink";
import { ShellCmd } from "@/components/ShellCmd";

const MIRROR_PRESETS = [
  { label: "gh-proxy.com", url: "https://gh-proxy.com/" },
  { label: "ghfast.top", url: "https://ghfast.top/" },
  { label: "moeyy.xyz", url: "https://github.moeyy.xyz/" },
];

function fmtBytes(n: number): string {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let v = n, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return `${v.toFixed(v < 10 ? 1 : 0)} ${u[i]}`;
}

interface NodeStats {
  tcpConns: number;
  udpConns: number;
  bytesIn: number;
  bytesOut: number;
}
import { toast } from "sonner";

function isOnline(n: NodeInfo): boolean {
  if (!n.last_seen_at) return false;
  return Date.now() - new Date(n.last_seen_at).getTime() < 15_000;
}

interface MetricRowProps {
  icon: React.ReactNode;
  label: string;
  value: string;
  pct: number | null;
  barColor: string;
}

function MetricRow({ icon, label, value, pct, barColor }: MetricRowProps) {
  return (
    <div className="space-y-1">
      <div className="flex items-center gap-1.5">
        <span className="text-muted-foreground flex-shrink-0">{icon}</span>
        <span className="text-sm text-foreground">
          {label} <span className="text-muted-foreground">{value}</span>
        </span>
      </div>
      <div className="h-[3px] w-full rounded-full bg-muted">
        <div
          className={`h-full rounded-full ${barColor} transition-all duration-500`}
          style={{ width: `${Math.min(100, Math.max(0, pct ?? 0))}%` }}
        />
      </div>
    </div>
  );
}

function fmtSpeed(bps: number): string {
  return `${fmtBytes(bps)}/s`;
}

function NodeCard({
  node,
  stats,
  speed,
}: {
  node: NodeInfo;
  stats: NodeStats | undefined;
  speed: NodeSpeed | undefined;
}) {
  const navigate = useNavigate();
  const online = isOnline(node);
  const hb = node.last_heartbeat as Record<string, unknown> | null;

  const cpuPct = typeof hb?.cpu_pct === "number" ? hb.cpu_pct : null;
  const memUsed = typeof hb?.mem_used_bytes === "number" ? hb.mem_used_bytes : 0;
  const memTotal = typeof hb?.mem_total_bytes === "number" ? hb.mem_total_bytes : 0;
  const memPct = memTotal > 0 ? (memUsed / memTotal) * 100 : null;
  const activeConns = typeof hb?.active_connections === "number" ? hb.active_connections : null;

  const cpuLabel = cpuPct !== null ? `${cpuPct.toFixed(1)}%` : "—";
  const memLabel = memTotal > 0 ? `${fmtBytes(memUsed)} / ${fmtBytes(memTotal)} (${memPct!.toFixed(1)}%)` : "—";

  return (
    <div
      className="flex flex-col gap-3.5 rounded-xl border bg-card p-4 dark:border-white/[0.07] dark:bg-[#191919] cursor-pointer"
      onClick={() => navigate(`/nodes/${node.id}`)}
    >
      {/* Header */}
      <div className="flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          <span
            className={`h-1.5 w-1.5 flex-shrink-0 rounded-full ${online ? "bg-emerald-500" : "bg-muted-foreground/40"}`}
          />
          <span className="truncate text-sm font-semibold text-foreground">
            {node.hostname || node.id}
          </span>
        </div>
        <span className="flex-shrink-0 tabular-nums text-sm text-muted-foreground">
          {node.last_seen_at ? timeAgo(node.last_seen_at) : "从未"}
        </span>
      </div>

      {/* Metrics */}
      <div className="space-y-2.5">
        <MetricRow
          icon={<Cpu className="h-3.5 w-3.5" />}
          label="CPU"
          value={cpuLabel}
          pct={cpuPct}
          barColor="bg-sky-500"
        />
        <MetricRow
          icon={<MemoryStick className="h-3.5 w-3.5" />}
          label="内存"
          value={memLabel}
          pct={memPct}
          barColor="bg-amber-500"
        />
      </div>

      {/* Connections + Traffic */}
      <div className="space-y-1.5">
        <div className="flex items-center gap-1.5 text-sm">
          <Network className="h-3.5 w-3.5 flex-shrink-0 text-muted-foreground" />
          <span className="text-muted-foreground">连接</span>
          <span className="ml-auto tabular-nums text-foreground">
            {stats
              ? `TCP ${stats.tcpConns} · UDP ${stats.udpConns}`
              : activeConns !== null ? String(activeConns) : "—"}
          </span>
        </div>
        <div className="flex items-center gap-1.5 text-sm">
          <DatabaseZap className="h-3.5 w-3.5 flex-shrink-0 text-muted-foreground" />
          <span className="text-muted-foreground">总流量</span>
          <span className="ml-auto tabular-nums text-foreground">
            {stats ? `↓ ${fmtBytes(stats.bytesIn)} · ↑ ${fmtBytes(stats.bytesOut)}` : "—"}
          </span>
        </div>
        <div className="flex items-center gap-1.5 text-sm">
          <Network className="h-3.5 w-3.5 flex-shrink-0 text-muted-foreground" />
          <span className="text-muted-foreground">实时网速</span>
          <span className="ml-auto tabular-nums text-foreground">
            {speed
              ? `↓ ${fmtSpeed(speed.rx_bps)} · ↑ ${fmtSpeed(speed.tx_bps)}`
              : online ? "—" : <span className="text-muted-foreground/50">离线</span>}
          </span>
        </div>
      </div>

      <div className="border-t pt-3">
        <span className="text-[11px] text-muted-foreground">{node.version || "—"}</span>
      </div>
    </div>
  );
}

export default function NodesPage() {
  const { data: nodes = [], mutate: refreshNodes } = useSWR("nodes", Api.listNodes, {
    onError: (e) => toast.error(e instanceof ApiError ? e.message : String(e)),
  });
  const { data: serverInfo } = useSWR("server-info", Api.serverInfo);
  const { data: allForwards = [] } = useSWR("forwards", Api.listForwards);
  const nodeSpeeds = useNodeSpeeds();

  const nodeStats = useMemo<Record<string, NodeStats>>(() => {
    const map: Record<string, NodeStats> = {};
    for (const f of allForwards) {
      const nodeIds = new Set(f.ports.map((p) => p.node_id));
      for (const nodeId of nodeIds) {
        if (!map[nodeId]) map[nodeId] = { tcpConns: 0, udpConns: 0, bytesIn: 0, bytesOut: 0 };
        // 双协议时连接数按协议聚合到对应桶；同 forward 同时计入 TCP+UDP。
        const protos = f.protocols ?? [];
        if (protos.includes("tcp")) map[nodeId].tcpConns += f.active_connections;
        if (protos.includes("udp")) map[nodeId].udpConns += f.active_connections;
        map[nodeId].bytesIn += f.in_flow_bytes;
        map[nodeId].bytesOut += f.out_flow_bytes;
      }
    }
    return map;
  }, [allForwards]);
  const [createOpen, setCreateOpen] = useState(false);
  const [created, setCreated] = useState<{ id: string; enrollment_token: string } | null>(null);
  const [search, setSearch] = useState("");

  // form
  const [hostname, setHostname] = useState("");
  const [portRangeStart, setPortRangeStart] = useState<number>(30000);
  const [portRangeEnd, setPortRangeEnd] = useState<number>(39999);

  const portRangeValid =
    Number.isInteger(portRangeStart) &&
    Number.isInteger(portRangeEnd) &&
    portRangeStart >= 1 &&
    portRangeEnd <= 65535 &&
    portRangeStart <= portRangeEnd;
  const formValid = portRangeValid;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!formValid) return;
    try {
      const resp = await Api.createNode({
        hostname: hostname.trim() || undefined,
        port_range_start: portRangeStart,
        port_range_end: portRangeEnd,
      });
      setCreated(resp);
      setCreateOpen(false);
      setHostname("");
      setPortRangeStart(30000);
      setPortRangeEnd(39999);
      refreshNodes();
      generateSetup(resp.id);
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e));
    }
  };

  const { setupUrl, mirrorUrl, setMirrorUrl, cmdCopied, generateSetup, copyCmd } =
    useSetupLink(serverInfo?.enroll_endpoint);
  const createdNode = created ? nodes.find((n) => n.id === created.id) : null;
  const nodeOnline = createdNode ? isOnline(createdNode) : false;

  const q = search.trim().toLowerCase();
  const filtered = nodes.filter((n) => {
    if (!q) return true;
    return (
      n.id.toLowerCase().includes(q) ||
      (n.hostname ?? "").toLowerCase().includes(q) ||
      n.server_ips.some((ip) => ip.toLowerCase().includes(q))
    );
  });

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold">节点</h1>
        <Dialog open={createOpen} onOpenChange={setCreateOpen}>
          <DialogTrigger asChild>
            <Button>
              <Plus className="mr-2 h-4 w-4" /> 新建节点
            </Button>
          </DialogTrigger>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>新建节点</DialogTitle>
            </DialogHeader>
            <form onSubmit={submit} className="space-y-4">
              <div className="space-y-2">
                <Label htmlFor="hostname">名称</Label>
                <Input
                  id="hostname"
                  value={hostname}
                  onChange={(e) => setHostname(e.target.value)}
                  placeholder="香港 01"
                />
              </div>
              <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                <div className="space-y-2">
                  <Label htmlFor="pr-start">可用端口范围 起始</Label>
                  <Input
                    id="pr-start"
                    type="number"
                    min={1}
                    max={65535}
                    value={portRangeStart}
                    onChange={(e) => setPortRangeStart(parseInt(e.target.value, 10))}
                    required
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="pr-end">可用端口范围 结束</Label>
                  <Input
                    id="pr-end"
                    type="number"
                    min={1}
                    max={65535}
                    value={portRangeEnd}
                    onChange={(e) => setPortRangeEnd(parseInt(e.target.value, 10))}
                    required
                  />
                </div>
              </div>
              {!portRangeValid && (
                <p className="text-sm text-destructive">
                  端口范围无效：要求 1 ≤ 起始 ≤ 结束 ≤ 65535
                </p>
              )}
              <DialogFooter>
                <Button type="submit" disabled={!formValid}>
                  创建
                </Button>
              </DialogFooter>
            </form>
          </DialogContent>
        </Dialog>
      </div>

      <Dialog open={!!created} onOpenChange={(o) => !o && setCreated(null)}>
        <DialogContent className="sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>安装命令</DialogTitle>
          </DialogHeader>
          {created && (
            <div className="space-y-4">
              <div>
                <Label className="text-sm uppercase text-muted-foreground">一键安装命令</Label>
                <div className="mt-2 mb-2 space-y-1">
                  <Input
                    placeholder="GitHub 镜像前缀（可选，如 https://gh-proxy.com/）"
                    value={mirrorUrl}
                    onChange={(e) => setMirrorUrl(e.target.value)}
                    className="h-8 text-xs"
                  />
                  <div className="flex flex-wrap gap-1">
                    {MIRROR_PRESETS.map((preset) => (
                      <button
                        key={preset.url}
                        type="button"
                        onClick={() => setMirrorUrl(mirrorUrl === preset.url ? "" : preset.url)}
                        className={`px-2 py-0.5 rounded text-xs border transition-colors ${
                          mirrorUrl === preset.url
                            ? "bg-primary text-primary-foreground border-primary"
                            : "bg-muted text-muted-foreground border-border hover:bg-accent hover:text-accent-foreground"
                        }`}
                      >
                        {preset.label}
                      </button>
                    ))}
                  </div>
                </div>
                <pre className="whitespace-pre-wrap break-all rounded-md border bg-muted p-3 text-xs">
                  <ShellCmd setupUrl={setupUrl} mirrorUrl={mirrorUrl} />
                </pre>
                <Button variant="outline" size="sm" className="mt-2" onClick={copyCmd}>
                  {cmdCopied ? (
                    <Check className="mr-2 h-4 w-4" />
                  ) : (
                    <Copy className="mr-2 h-4 w-4" />
                  )}
                  {cmdCopied ? "已复制" : "复制命令"}
                </Button>
              </div>
            </div>
          )}
          <DialogFooter>
            <div className="flex w-full items-center justify-between">
              <div className="flex items-center gap-2 text-sm">
                {nodeOnline ? (
                  <>
                    <Check className="h-4 w-4 text-emerald-500" />
                    <span className="text-emerald-500">节点已上线</span>
                  </>
                ) : (
                  <>
                    <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
                    <span className="text-muted-foreground">等待节点上线…</span>
                  </>
                )}
              </div>
              <Button onClick={() => setCreated(null)}>完成</Button>
            </div>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <div className="relative max-w-xs">
        <Search className="pointer-events-none absolute left-2.5 top-2.5 h-3.5 w-3.5 text-muted-foreground" />
        <Input
          placeholder="搜索节点名称、地址…"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          className="h-9 pl-8"
        />
      </div>

      {nodes.length === 0 ? (
        <p className="text-sm text-muted-foreground">暂无节点。</p>
      ) : filtered.length === 0 ? (
        <p className="text-sm text-muted-foreground">无匹配节点。</p>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
          {filtered.map((n) => (
            <NodeCard
              key={n.id}
              node={n}
              stats={nodeStats[n.id]}
              speed={nodeSpeeds[n.id]}
            />
          ))}
        </div>
      )}
    </div>
  );
}
