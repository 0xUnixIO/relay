import { useState, useMemo, useRef } from "react";
import useSWR from "swr";
import { Link } from "react-router-dom";
import { Search, Settings2, X, AlertTriangle, WifiOff, AlertCircle } from "lucide-react";
import { Api, type NodeInfo, type HeartbeatSample } from "@/lib/api";
import { useNodeSpeeds } from "@/hooks/useNodeSpeeds";
import { fmtBytes } from "@/lib/utils";
import { cn } from "@/lib/utils";
import {
  AreaChart,
  Area,
  ResponsiveContainer,
  Tooltip as ReTooltip,
} from "recharts";

// ─── 类型 ───────────────────────────────────────────────────────────────────

type StatusLevel = "ok" | "warn" | "alert";
type FilterKey = "all" | "Asia" | "N. America" | "Europe" | "Other" | "alert";

// ─── 辅助：在线判断（与其他页保持一致） ─────────────────────────────────────

function isOnline(n: NodeInfo): boolean {
  if (!n.last_seen_at) return false;
  return Date.now() - new Date(n.last_seen_at).getTime() < 15_000;
}

// ─── 辅助：从 last_heartbeat 提取 CPU / MEM ──────────────────────────────────

function getMetrics(node: NodeInfo): { cpuPct: number; memPct: number } {
  const hb = node.last_heartbeat as HeartbeatSample | null;
  if (!hb) return { cpuPct: 0, memPct: 0 };
  const cpuPct = hb.cpu_pct ?? 0;
  const memPct =
    hb.mem_total_bytes > 0
      ? (hb.mem_used_bytes / hb.mem_total_bytes) * 100
      : 0;
  return { cpuPct, memPct };
}

// ─── 辅助：节点状态 ───────────────────────────────────────────────────────────

function getStatus(node: NodeInfo): StatusLevel {
  if (!isOnline(node)) return "alert";
  const { cpuPct, memPct } = getMetrics(node);
  if (cpuPct > 85 || memPct > 85) return "alert";
  if (cpuPct > 65 || memPct > 65) return "warn";
  return "ok";
}

// ─── 辅助：地区检测 ──────────────────────────────────────────────────────────

const REGION_KW: Record<string, string[]> = {
  Asia: ["asia","as","ap","cn","hk","tw","jp","sg","kr","th","my","vn","in","ph","id","tokyo","osaka","singapore","hongkong","beijing","shanghai","seoul","bangkok","mumbai","khabarovsk"],
  "N. America": ["na","us","ca","usa","canada","america","seattle","dallas","fremont","losangeles","la","nyc","newyork","chicago","miami","atlanta","denver","vancouver","toronto","montreal","ashburn","washington","beauharnois","calgary"],
  Europe: ["eu","europe","de","fr","nl","gb","uk","pl","ru","ie","at","se","fi","frankfurt","paris","amsterdam","london","warsaw","dublin","moscow","vienna","stockholm","falkenstein","nuremberg","roubaix"],
};

function detectRegion(node: NodeInfo): string {
  const text = [...node.tags, node.hostname, ...node.server_ips].join(" ").toLowerCase();
  for (const [region, kws] of Object.entries(REGION_KW)) {
    if (kws.some((k) => text.includes(k))) return region;
  }
  return "Other";
}

// ─── 辅助：ISP 检测 ──────────────────────────────────────────────────────────

const ISP_MAP: [string, string][] = [
  ["aws","AWS"], ["alibaba","Alibaba Cloud"], ["aliyun","Alibaba Cloud"],
  ["tencent","Tencent Cloud"], ["huawei","Huawei Cloud"], ["jdcloud","JD Cloud"],
  ["azure","Azure"], ["gcp","Google Cloud"], ["oracle","Oracle Cloud"],
  ["digitalocean","DigitalOcean"], ["vultr","Vultr"], ["linode","Linode/Akamai"],
  ["akamai","Akamai"], ["hetzner","Hetzner"], ["ovh","OVH"], ["scaleway","Scaleway"],
  ["ibm","IBM Cloud"], ["cogent","Cogent"], ["hurricane","Hurricane Electric"],
  ["racknerd","Racknerd"], ["bandwagon","BandwagonHost"], ["bgp","BGP"],
  ["cn2","CN2 GIA"], ["telus","Telus"], ["ntt","NTT"],
];

function detectISP(node: NodeInfo): string {
  const text = [...node.tags, node.hostname].join(" ").toLowerCase();
  for (const [k, name] of ISP_MAP) {
    if (text.includes(k)) return name;
  }
  return node.tags[0] ?? "—";
}

// ─── 辅助：国旗代码 ──────────────────────────────────────────────────────────

const COUNTRY_CODES: Record<string, string> = {
  us:"US", ca:"CA", cn:"CN", hk:"HK", tw:"TW", jp:"JP", sg:"SG",
  kr:"KR", de:"DE", fr:"FR", nl:"NL", gb:"GB", uk:"GB", ie:"IE",
  pl:"PL", ru:"RU", au:"AU", in:"IN", br:"BR", th:"TH", vn:"VN",
};

function detectFlag(node: NodeInfo): string {
  const tags = node.tags.map((t) => t.toLowerCase());
  for (const [code, flag] of Object.entries(COUNTRY_CODES)) {
    if (tags.includes(code)) return flag;
  }
  const name = node.hostname.toLowerCase();
  for (const [code, flag] of Object.entries(COUNTRY_CODES)) {
    if (name.includes(`-${code}`) || name.includes(`${code}-`) || name.startsWith(`${code}`)) return flag;
  }
  return "??";
}

// ─── 辅助：地理坐标（用于迷你地图） ─────────────────────────────────────────

const LOC_COORDS: Record<string, [number, number]> = {
  US: [39, -98], CA: [54, -106], CN: [35, 105], HK: [22.3, 114.2],
  JP: [36, 138], SG: [1.3, 103.8], KR: [37, 128], DE: [51, 10],
  FR: [47, 2], NL: [52.4, 4.9], GB: [54, -2], IE: [53.3, -6.3],
  PL: [52, 20], RU: [56, 37], AU: [-25, 133], IN: [21, 78],
  BR: [-15, -47], TW: [23.7, 121], TH: [15, 101], VN: [16, 106],
};

function llToPct(lat: number, lng: number): [number, number] {
  const x = ((lng + 180) / 360) * 100;
  const y = ((75 - lat) / 130) * 100;
  return [Math.max(2, Math.min(98, x)), Math.max(2, Math.min(98, y))];
}

// ─── 辅助：伪随机波形 SVG ─────────────────────────────────────────────────────

function seedRandom(seed: number) {
  let s = seed % 2147483647;
  if (s <= 0) s += 2147483646;
  return () => { s = (s * 16807) % 2147483647; return s / 2147483647; };
}

function waveformPath(seed: number, w: number, h: number): string {
  const rnd = seedRandom(seed);
  const n = 22;
  const pts: [number, number][] = [];
  let y = h / 2;
  for (let i = 0; i < n; i++) {
    y += (rnd() - 0.5) * h * 0.55;
    y = Math.max(h * 0.08, Math.min(h * 0.92, y));
    pts.push([(i / (n - 1)) * w, y]);
  }
  let d = `M ${pts[0][0].toFixed(1)} ${pts[0][1].toFixed(1)}`;
  for (let i = 1; i < pts.length; i++) {
    const [x0, y0] = pts[i - 1];
    const [x1, y1] = pts[i];
    const cx = (x0 + x1) / 2;
    d += ` Q ${cx.toFixed(1)} ${y0.toFixed(1)} ${x1.toFixed(1)} ${y1.toFixed(1)}`;
  }
  return d;
}

// ─── 常量：状态色 ─────────────────────────────────────────────────────────────

const STATUS_COLOR: Record<StatusLevel, string> = {
  ok:    "bg-emerald-500",
  warn:  "bg-amber-400",
  alert: "bg-red-500",
};

const STATUS_BORDER: Record<StatusLevel, string> = {
  ok:    "",
  warn:  "border-amber-400/60",
  alert: "border-red-500/60",
};

const BAR_COLOR: Record<StatusLevel, string> = {
  ok:    "#10b981",
  warn:  "#f59e0b",
  alert: "#ef4444",
};

// ─── 子组件：进度条（带斜纹填充） ────────────────────────────────────────────

function MetricBar({ label, pct, status }: { label: string; pct: number; status: StatusLevel }) {
  const color = BAR_COLOR[status];
  return (
    <div className="grid items-center gap-x-1.5" style={{ gridTemplateColumns: "28px 1fr 30px" }}>
      <span className="font-mono text-[10px] text-muted-foreground">{label}</span>
      <div className="h-[5px] rounded-[2px] bg-muted border border-border/40 overflow-hidden">
        <div
          className="h-full rounded-[2px]"
          style={{
            width: `${Math.min(100, pct)}%`,
            backgroundImage: `repeating-linear-gradient(45deg, ${color} 0 2px, transparent 2px 5px)`,
          }}
        />
      </div>
      <span className="font-mono text-[10px] text-right tabular-nums"
            style={{ color }}>
        {pct.toFixed(0)}%
      </span>
    </div>
  );
}

// ─── 子组件：节点卡片 ─────────────────────────────────────────────────────────

interface NodeCardProps {
  node: NodeInfo;
  speed: { rx: number; tx: number } | undefined;
}

function NodeCard({ node, speed }: NodeCardProps) {
  const status = getStatus(node);
  const { cpuPct, memPct } = getMetrics(node);
  const online = isOnline(node);
  const isp = detectISP(node);
  const flag = detectFlag(node);
  const seed = node.id.split("").reduce((acc, c) => acc + c.charCodeAt(0), 0);

  const wavePath = waveformPath(seed, 200, 34);
  const waveColor = status === "alert" ? "#ef4444" : status === "warn" ? "#f59e0b" : "#10b981";

  const cpuStatus: StatusLevel = cpuPct > 85 ? "alert" : cpuPct > 65 ? "warn" : "ok";
  const memStatus: StatusLevel = memPct > 85 ? "alert" : memPct > 65 ? "warn" : "ok";

  return (
    <Link
      to={`/nodes/${node.id}`}
      className={cn(
        "block rounded-lg border bg-card p-3 flex flex-col gap-2 min-h-[164px]",
        "hover:shadow-md hover:border-primary/40 transition-all duration-150",
        status !== "ok" && STATUS_BORDER[status],
      )}
    >
      {/* 头部：旗帜 + 名称 + 状态点 */}
      <div className="flex items-center gap-1.5">
        <span className="font-mono text-[10px] px-1 py-0.5 rounded-[2px] bg-muted border border-border/50 shrink-0 leading-none">
          {flag}
        </span>
        <span className="font-mono text-[13px] font-semibold truncate flex-1 leading-none">
          {node.hostname}
        </span>
        <span className={cn("h-2 w-2 rounded-full shrink-0", STATUS_COLOR[status])} />
      </div>

      {/* ISP */}
      <div className="font-mono text-[10px] text-muted-foreground truncate -mt-1">
        {isp}
      </div>

      {/* 指标条 */}
      {online ? (
        <div className="flex flex-col gap-1.5">
          <MetricBar label="CPU" pct={cpuPct} status={cpuStatus} />
          <MetricBar label="MEM" pct={memPct} status={memStatus} />
        </div>
      ) : (
        <div className="flex items-center gap-1.5 text-[11px] text-red-500 font-mono py-1">
          <WifiOff className="h-3 w-3" />
          OFFLINE
        </div>
      )}

      {/* 波形 */}
      <div className="mt-auto">
        <svg
          viewBox={`0 0 200 34`}
          preserveAspectRatio="none"
          className="w-full"
          style={{ height: 32 }}
        >
          <path
            d={`${wavePath} L 200 34 L 0 34 Z`}
            fill={waveColor}
            fillOpacity={0.07}
          />
          <path
            d={wavePath}
            fill="none"
            stroke={waveColor}
            strokeWidth={1.2}
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      </div>

      {/* 页脚：网速 */}
      <div className="flex justify-between items-center border-t border-dashed border-border/40 pt-1.5 font-mono text-[10px] text-muted-foreground">
        {speed ? (
          <>
            <span>↓ {fmtBytes(speed.rx)}/s</span>
            <span className="text-foreground">↑ {fmtBytes(speed.tx)}/s</span>
          </>
        ) : (
          <span className="text-muted-foreground/50">—</span>
        )}
      </div>
    </Link>
  );
}

// ─── 子组件：迷你世界地图 ────────────────────────────────────────────────────

function MiniMap({ nodes }: { nodes: NodeInfo[] }) {
  const pins = useMemo(() => {
    return nodes.map((node) => {
      const flag = detectFlag(node);
      const coords = LOC_COORDS[flag] ?? [0, 0];
      const [x, y] = llToPct(coords[0], coords[1]);
      const status = getStatus(node);
      const color = status === "alert" ? "#ef4444" : status === "warn" ? "#f59e0b" : "#10b981";
      return { key: node.id, x, y, color };
    });
  }, [nodes]);

  return (
    <div className="relative h-[100px]">
      {/* 简化大陆轮廓 */}
      <svg
        viewBox="0 0 1000 460"
        preserveAspectRatio="none"
        className="absolute inset-0 w-full h-full"
      >
        <g fill="hsl(var(--muted))" stroke="hsl(var(--border))" strokeWidth="1.5" opacity="0.6">
          <path d="M120 110 Q180 70 260 90 Q330 80 380 130 Q420 180 380 220 Q320 250 260 240 Q180 240 140 200 Q90 160 120 110 Z" />
          <path d="M180 270 Q220 260 250 290 Q280 330 260 370 Q230 410 200 390 Q170 360 180 320 Z" />
          <path d="M430 100 Q500 70 590 95 Q660 100 700 140 Q720 200 680 240 Q620 270 560 260 Q500 250 460 220 Q420 170 430 100 Z" />
          <path d="M580 270 Q640 260 680 290 Q720 340 700 410 Q660 440 620 420 Q580 380 580 320 Z" />
          <path d="M720 170 Q780 150 840 180 Q900 210 880 260 Q830 280 780 270 Q730 240 720 170 Z" />
        </g>
      </svg>
      {/* 节点 pins */}
      {pins.map((p) => (
        <div
          key={p.key}
          className="absolute rounded-full border-[1.5px] border-background"
          style={{
            width: 7,
            height: 7,
            left: `${p.x}%`,
            top: `${p.y}%`,
            background: p.color,
            transform: "translate(-50%, -50%)",
          }}
        />
      ))}
    </div>
  );
}

// ─── 子组件：流量图 Tooltip ───────────────────────────────────────────────────

function TrafficTooltip({
  active,
  payload,
  label,
}: {
  active?: boolean;
  payload?: Array<{ value: number; name: string }>;
  label?: string;
}) {
  if (!active || !payload?.length) return null;
  return (
    <div className="rounded border bg-background px-2.5 py-1.5 text-xs shadow-md space-y-0.5 font-mono">
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

// ─── 子组件：筛选药丸 ────────────────────────────────────────────────────────

interface FilterPillProps {
  label: string;
  count: number;
  active: boolean;
  onClick: () => void;
}

function FilterPill({ label, count, active, onClick }: FilterPillProps) {
  return (
    <button
      onClick={onClick}
      className={cn(
        "flex items-center gap-1.5 px-3 py-1.5 rounded-full border text-sm transition-all",
        active
          ? "bg-foreground text-background border-foreground font-medium"
          : "bg-transparent border-border text-muted-foreground hover:border-foreground/40 hover:text-foreground",
      )}
    >
      {label}
      <span className={cn("font-mono text-[10px]", active ? "opacity-70" : "opacity-50")}>
        {count}
      </span>
    </button>
  );
}

// ─── 主页面 ──────────────────────────────────────────────────────────────────

type DensitySetting = "cozy" | "dense";

export default function NodeOverview() {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<FilterKey>("all");
  const [density, setDensity] = useState<DensitySetting>("dense");
  const [tweaksOpen, setTweaksOpen] = useState(false);
  const tweaksRef = useRef<HTMLDivElement>(null);

  const { data: nodes = [], isLoading } = useSWR("nodes-overview", Api.listNodes, {
    refreshInterval: 10_000,
  });
  const speeds = useNodeSpeeds();
  const { data: trafficStats = [] } = useSWR(
    "global-traffic-24h",
    () => Api.getGlobalTrafficStats("24h"),
    { refreshInterval: 60_000 },
  );

  // ── 派生数据 ──

  const enriched = useMemo(
    () =>
      nodes.map((node) => ({
        node,
        status: getStatus(node),
        region: detectRegion(node),
        speed: speeds[node.id]
          ? { rx: speeds[node.id].rx_bps, tx: speeds[node.id].tx_bps }
          : undefined,
      })),
    [nodes, speeds],
  );

  const stats = useMemo(() => {
    const operational = enriched.filter((e) => e.status === "ok").length;
    const alertCount = enriched.filter((e) => e.status === "alert").length;
    const warnCount = enriched.filter((e) => e.status === "warn").length;
    return { operational, alertCount, warnCount, total: enriched.length };
  }, [enriched]);

  const alertNodes = useMemo(
    () => enriched.filter((e) => e.status === "alert" || e.status === "warn"),
    [enriched],
  );

  const filteredEnriched = useMemo(() => {
    let list = enriched;
    if (filter === "alert") list = list.filter((e) => e.status === "alert" || e.status === "warn");
    else if (filter !== "all") list = list.filter((e) => e.region === filter);
    if (query.trim()) {
      const q = query.toLowerCase();
      list = list.filter(
        (e) =>
          e.node.hostname.toLowerCase().includes(q) ||
          e.node.tags.some((t) => t.toLowerCase().includes(q)) ||
          e.node.server_ips.some((ip) => ip.includes(q)),
      );
    }
    return list;
  }, [enriched, filter, query]);

  // 分地区分组
  const grouped = useMemo(() => {
    const regionOrder = ["Asia", "N. America", "Europe", "Other"];
    const map: Record<string, typeof filteredEnriched> = {};
    for (const e of filteredEnriched) {
      (map[e.region] ??= []).push(e);
    }
    return regionOrder
      .filter((r) => map[r]?.length)
      .map((r) => ({ region: r, items: map[r] }));
  }, [filteredEnriched]);

  // 地区药丸计数
  const regionCounts = useMemo(() => {
    const counts: Record<string, number> = {};
    for (const e of enriched) {
      counts[e.region] = (counts[e.region] ?? 0) + 1;
    }
    return counts;
  }, [enriched]);

  const trafficChartData = useMemo(
    () =>
      trafficStats.map((b) => ({
        ts: new Date(b.ts).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" }),
        in: b.bytes_in,
        out: b.bytes_out,
      })),
    [trafficStats],
  );

  const gridCols = density === "dense"
    ? "grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6"
    : "grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5";

  const REGIONS: FilterKey[] = ["Asia", "N. America", "Europe", "Other"];

  return (
    <div className="space-y-4 pb-10">
      {/* ── 告警 Banner ── */}
      {alertNodes.length > 0 && (
        <div className="flex flex-wrap items-center gap-3 rounded-lg border border-red-500/40 bg-red-500/5 px-4 py-3">
          <AlertTriangle className="h-4 w-4 text-red-500 shrink-0" />
          <span className="font-mono text-sm font-semibold text-red-500">
            {alertNodes.length} 个节点需要关注
          </span>
          <span className="text-xs text-muted-foreground hidden sm:block">
            点击下方卡片查看详情
          </span>
          <div className="flex flex-wrap gap-1.5 ml-auto">
            {alertNodes.slice(0, 5).map(({ node, status }) => (
              <Link
                key={node.id}
                to={`/nodes/${node.id}`}
                className={cn(
                  "font-mono text-[11px] px-2 py-0.5 rounded-full border",
                  status === "alert"
                    ? "border-red-500/60 text-red-500"
                    : "border-amber-400/60 text-amber-500",
                )}
              >
                {node.hostname}
              </Link>
            ))}
            {alertNodes.length > 5 && (
              <span className="font-mono text-[11px] px-2 py-0.5 rounded-full border border-border/50 text-muted-foreground">
                +{alertNodes.length - 5}
              </span>
            )}
          </div>
        </div>
      )}

      {/* ── 顶部统计条 ── */}
      <div className="grid gap-3" style={{ gridTemplateColumns: "auto 1fr 1fr" }}>
        {/* 计数器 */}
        <div className="rounded-lg border bg-card p-3 flex flex-col gap-2 justify-center min-w-[130px]">
          <div className="flex items-center gap-2">
            <span className="h-2.5 w-2.5 rounded-full bg-emerald-500 shrink-0" />
            <span className="font-mono font-bold text-2xl leading-none">{stats.operational}</span>
            <span className="font-mono text-[10px] text-muted-foreground uppercase tracking-wider ml-auto">正常</span>
          </div>
          <div className="flex items-center gap-2">
            <span className="h-2.5 w-2.5 rounded-full bg-amber-400 shrink-0" />
            <span className="font-mono font-bold text-2xl leading-none">{stats.warnCount}</span>
            <span className="font-mono text-[10px] text-muted-foreground uppercase tracking-wider ml-auto">警告</span>
          </div>
          <div className="flex items-center gap-2">
            <span className="h-2.5 w-2.5 rounded-full bg-red-500 shrink-0" />
            <span className="font-mono font-bold text-2xl leading-none">{stats.alertCount}</span>
            <span className="font-mono text-[10px] text-muted-foreground uppercase tracking-wider ml-auto">离线</span>
          </div>
        </div>

        {/* 24h 流量图 */}
        <div className="rounded-lg border bg-card px-4 py-3">
          <div className="flex items-baseline justify-between mb-2">
            <span className="font-mono text-sm font-semibold">24h 汇总流量</span>
            <span className="font-mono text-[10px] text-muted-foreground">
              {trafficChartData.length > 0
                ? `${new Date(trafficStats[0]?.ts).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })} – 现在`
                : ""}
            </span>
          </div>
          {trafficChartData.length > 0 ? (
            <div style={{ height: 88 }}>
              <ResponsiveContainer width="100%" height="100%">
                <AreaChart data={trafficChartData} margin={{ top: 2, right: 2, bottom: 0, left: 2 }}>
                  <defs>
                    <linearGradient id="noc-in" x1="0" y1="0" x2="0" y2="1">
                      <stop offset="0%" stopColor="#10b981" stopOpacity={0.25} />
                      <stop offset="100%" stopColor="#10b981" stopOpacity={0} />
                    </linearGradient>
                    <linearGradient id="noc-out" x1="0" y1="0" x2="0" y2="1">
                      <stop offset="0%" stopColor="#6366f1" stopOpacity={0.2} />
                      <stop offset="100%" stopColor="#6366f1" stopOpacity={0} />
                    </linearGradient>
                  </defs>
                  <ReTooltip content={(p) => <TrafficTooltip {...(p as any)} />} cursor={false} />
                  <Area
                    type="monotone"
                    dataKey="in"
                    name="↓ 入站"
                    stroke="#10b981"
                    strokeWidth={1.4}
                    fill="url(#noc-in)"
                    isAnimationActive={false}
                    dot={false}
                  />
                  <Area
                    type="monotone"
                    dataKey="out"
                    name="↑ 出站"
                    stroke="#6366f1"
                    strokeWidth={1.2}
                    strokeDasharray="3 3"
                    fill="url(#noc-out)"
                    isAnimationActive={false}
                    dot={false}
                  />
                </AreaChart>
              </ResponsiveContainer>
            </div>
          ) : (
            <div className="h-[88px] flex items-center justify-center text-xs text-muted-foreground font-mono">
              暂无流量数据
            </div>
          )}
        </div>

        {/* 迷你地图 */}
        <div className="rounded-lg border bg-card px-4 py-3">
          <div className="flex items-baseline justify-between mb-2">
            <span className="font-mono text-sm font-semibold">分布</span>
            <span className="font-mono text-[10px] text-muted-foreground">
              {stats.total} 节点 · {Object.keys(regionCounts).length} 地区
            </span>
          </div>
          <MiniMap nodes={nodes} />
        </div>
      </div>

      {/* ── 搜索 + 筛选 ── */}
      <div className="flex flex-wrap gap-2 items-center">
        <div className="relative">
          <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="搜索节点、IP、标签…"
            className="pl-8 pr-3 py-2 text-sm rounded-lg border bg-background focus:outline-none focus:ring-1 focus:ring-ring w-48 font-mono"
          />
          {query && (
            <button
              onClick={() => setQuery("")}
              className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
            >
              <X className="h-3.5 w-3.5" />
            </button>
          )}
        </div>
        <div className="flex flex-wrap gap-1.5">
          <FilterPill label="全部" count={stats.total} active={filter === "all"} onClick={() => setFilter("all")} />
          {REGIONS.filter((r) => regionCounts[r]).map((r) => (
            <FilterPill
              key={r}
              label={r}
              count={regionCounts[r] ?? 0}
              active={filter === r}
              onClick={() => setFilter(filter === r ? "all" : r)}
            />
          ))}
          {stats.alertCount + stats.warnCount > 0 && (
            <FilterPill
              label="⚠ 告警"
              count={stats.alertCount + stats.warnCount}
              active={filter === "alert"}
              onClick={() => setFilter(filter === "alert" ? "all" : "alert")}
            />
          )}
        </div>
        {/* 查询结果提示 */}
        {query && (
          <span className="font-mono text-xs text-muted-foreground">
            {filteredEnriched.length} 个匹配
          </span>
        )}
      </div>

      {/* ── 卡片网格（按地区分组） ── */}
      {isLoading ? (
        <div className="flex items-center justify-center h-40 text-muted-foreground font-mono text-sm">
          加载中…
        </div>
      ) : filteredEnriched.length === 0 ? (
        <div className="flex items-center justify-center h-40 text-muted-foreground font-mono text-sm">
          <AlertCircle className="h-4 w-4 mr-2" />
          没有匹配的节点
        </div>
      ) : (
        <div className="space-y-6">
          {grouped.map(({ region, items }) => (
            <div key={region}>
              <div className="flex items-baseline gap-3 mb-3">
                <h2 className="font-mono font-bold text-xl">{region}</h2>
                <span className="font-mono text-xs text-muted-foreground">
                  {items.length} 节点
                </span>
                <div className="flex-1 border-b border-dashed border-border/40" />
              </div>
              <div className={cn("grid gap-2.5", gridCols)}>
                {items.map(({ node, speed }) => (
                  <NodeCard key={node.id} node={node} speed={speed} />
                ))}
              </div>
            </div>
          ))}
        </div>
      )}

      {/* ── Tweaks FAB ── */}
      <div className="fixed right-5 bottom-5 z-40 flex flex-col items-end gap-2">
        {tweaksOpen && (
          <div
            ref={tweaksRef}
            className="rounded-xl border bg-card/90 backdrop-blur-sm shadow-xl p-4 w-64 flex flex-col gap-4"
          >
            <div className="flex items-center justify-between">
              <span className="font-mono font-semibold text-sm">显示设置</span>
              <button
                onClick={() => setTweaksOpen(false)}
                className="text-muted-foreground hover:text-foreground"
              >
                <X className="h-4 w-4" />
              </button>
            </div>
            <div className="space-y-2">
              <div className="font-mono text-[10px] text-muted-foreground uppercase tracking-wider">
                卡片密度
              </div>
              <div className="flex rounded-lg border overflow-hidden">
                <button
                  onClick={() => setDensity("cozy")}
                  className={cn(
                    "flex-1 py-1.5 text-sm transition-colors",
                    density === "cozy"
                      ? "bg-foreground text-background font-medium"
                      : "text-muted-foreground hover:text-foreground",
                  )}
                >
                  宽松 5列
                </button>
                <button
                  onClick={() => setDensity("dense")}
                  className={cn(
                    "flex-1 py-1.5 text-sm transition-colors border-l",
                    density === "dense"
                      ? "bg-foreground text-background font-medium"
                      : "text-muted-foreground hover:text-foreground",
                  )}
                >
                  紧凑 6列
                </button>
              </div>
            </div>
          </div>
        )}
        <button
          onClick={() => setTweaksOpen((o) => !o)}
          className="flex items-center gap-2 rounded-full border bg-card shadow-lg px-4 py-2 text-sm font-mono hover:shadow-xl transition-shadow"
        >
          <Settings2 className="h-4 w-4" />
          视图
        </button>
      </div>
    </div>
  );
}
