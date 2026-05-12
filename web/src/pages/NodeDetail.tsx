import { useEffect, useState } from "react";
import useSWR from "swr";
import { timeAgo, fmtBytes } from "@/lib/utils";
import { useParams, Link, useNavigate } from "react-router-dom";
import { ArrowLeft, RefreshCw, Copy, Check, Save, Plus, X, ArrowUp, ArrowDown, ArrowUpCircle, Trash2, MoreHorizontal } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { UpgradeNodeDialog } from "@/components/UpgradeNodeDialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Sparkline } from "@/components/Sparkline";
import {
  Api,
  ApiError,
  type NodeInfo,
  type RotateTokenResp,
} from "@/lib/api";
import { toast } from "sonner";
import { useSetupLink } from "@/hooks/useSetupLink";
import { ShellCmd } from "@/components/ShellCmd";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

function isOnline(n: NodeInfo): boolean {
  if (!n.last_seen_at) return false;
  return Date.now() - new Date(n.last_seen_at).getTime() < 15_000;
}



// 从累计计数器样本计算每秒增量
function rates(
  samples: { ts_unix_ms: number; bytes_in: number; bytes_out: number }[],
): { in: number[]; out: number[] } {
  const inR: number[] = [];
  const outR: number[] = [];
  for (let i = 1; i < samples.length; i++) {
    const dt = (samples[i].ts_unix_ms - samples[i - 1].ts_unix_ms) / 1000;
    if (dt <= 0) continue;
    inR.push(Math.max(0, (samples[i].bytes_in - samples[i - 1].bytes_in) / dt));
    outR.push(Math.max(0, (samples[i].bytes_out - samples[i - 1].bytes_out) / dt));
  }
  return { in: inR, out: outR };
}

export default function NodeDetail() {
  const { id = "" } = useParams();
  const { data: node, mutate: mutateNode } = useSWR(
    ["node", id],
    () => Api.getNode(id),
  );
  const { data: series } = useSWR(
    ["node-series", id],
    () => Api.getNodeSeries(id),
    { refreshInterval: 5000 },
  );
  const { data: allForwards = [] } = useSWR("forwards", Api.listForwards, {
    refreshInterval: 5000,
  });
  const { data: serverInfo } = useSWR("server-info", Api.serverInfo);

  const forwards = allForwards.filter((f) => f.ports.some((p) => p.node_id === id));
  const [rotated, setRotated] = useState<RotateTokenResp | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const navigate = useNavigate();
  const [upgradeOpen, setUpgradeOpen] = useState(false);
  const { setupUrl, mirrorUrl, setMirrorUrl, cmdCopied, generateSetup, copyCmd: copyReenrollCmd } =
    useSetupLink(serverInfo?.enroll_endpoint);
  const [ipsInput, setIpsInput] = useState<string[]>([]);
  const [saving, setSaving] = useState(false);
  const [prStart, setPrStart] = useState<number | "">("");
  const [prEnd, setPrEnd] = useState<number | "">("");
  const [ratioInput, setRatioInput] = useState<string>("");
  const [hostnameInput, setHostnameInput] = useState<string>("");
  const [metaExpiresAt, setMetaExpiresAt] = useState<string>("");
  const [metaPrice, setMetaPrice] = useState<string>("");
  const [metaWebsite, setMetaWebsite] = useState<string>("");
  const [listenStack, setListenStack] = useState<string>("dual");

  useEffect(() => {
    if (!node) return;
    setHostnameInput((cur) => cur === "" || cur === node.hostname ? node.hostname : cur);
  }, [node?.hostname]);

  // 同步 server_ips 编辑器：新数据落地时同步，但不覆盖用户正在编辑的内容
  useEffect(() => {
    if (!node) return;
    setIpsInput((current) => {
      const saved = node.server_ips ?? [];
      const isFirstLoad = current.length === 0;
      const matches =
        current.length === saved.length &&
        current.every((v, i) => v === saved[i]);
      return isFirstLoad || matches ? saved.slice() : current;
    });
  }, [node?.server_ips]);

  const savedIps = node?.server_ips ?? [];
  const trimmedIps = ipsInput.map((s) => s.trim()).filter((s) => s.length > 0);
  const ipsDirty = node
    ? trimmedIps.length !== savedIps.length ||
      trimmedIps.some((v, i) => v !== savedIps[i])
    : false;

  const updateIp = (idx: number, value: string) => {
    setIpsInput((cur) => cur.map((v, i) => (i === idx ? value : v)));
  };
  const removeIp = (idx: number) => {
    setIpsInput((cur) => cur.filter((_, i) => i !== idx));
  };
  const addIp = () => {
    setIpsInput((cur) => [...cur, ""]);
  };
  const moveIp = (idx: number, dir: -1 | 1) => {
    setIpsInput((cur) => {
      const j = idx + dir;
      if (j < 0 || j >= cur.length) return cur;
      const next = cur.slice();
      [next[idx], next[j]] = [next[j], next[idx]];
      return next;
    });
  };

  // 同步端口范围编辑器，规则同 server_ips 编辑器
  useEffect(() => {
    if (!node) return;
    setPrStart((cur) =>
      cur === "" || cur === node.port_range_start ? node.port_range_start : cur,
    );
    setPrEnd((cur) =>
      cur === "" || cur === node.port_range_end ? node.port_range_end : cur,
    );
  }, [node?.port_range_start, node?.port_range_end]);

  useEffect(() => {
    if (!node) return;
    setRatioInput((cur) => {
      const saved = String(node.traffic_ratio ?? 1.0);
      return cur === "" || cur === saved ? saved : cur;
    });
  }, [node?.traffic_ratio]);

  useEffect(() => {
    if (!node) return;
    setListenStack(node.listen_stack ?? "dual");
  }, [node?.listen_stack]);

  // 同步节点信息字段（expires_at / monthly_price / website）
  useEffect(() => {
    if (!node) return;
    setMetaExpiresAt((cur) => {
      const saved = node.expires_at ? node.expires_at.slice(0, 10) : "";
      return cur === "" || cur === saved ? saved : cur;
    });
    setMetaPrice((cur) => {
      const saved = node.monthly_price != null ? String(node.monthly_price) : "";
      return cur === "" || cur === saved ? saved : cur;
    });
    setMetaWebsite((cur) => cur === "" || cur === node.website ? node.website : cur);
  }, [node?.expires_at, node?.monthly_price, node?.website]);

  const prStartNum = typeof prStart === "number" ? prStart : NaN;
  const prEndNum = typeof prEnd === "number" ? prEnd : NaN;
  const prValid =
    Number.isInteger(prStartNum) &&
    Number.isInteger(prEndNum) &&
    prStartNum >= 1 &&
    prEndNum <= 65535 &&
    prStartNum <= prEndNum;
  const prDirty =
    !!node &&
    (prStartNum !== node.port_range_start || prEndNum !== node.port_range_end);

  const ratioNum = Number(ratioInput);
  const ratioValid = Number.isFinite(ratioNum) && ratioNum >= 0;
  const ratioDirty = !!node && Math.abs(ratioNum - node.traffic_ratio) > 1e-9;

  const metaExpiresAtSaved = node?.expires_at ? node.expires_at.slice(0, 10) : "";
  const metaPriceSaved = node?.monthly_price != null ? String(node.monthly_price) : "";
  const metaDirty = !!node && (
    metaExpiresAt !== metaExpiresAtSaved ||
    metaPrice !== metaPriceSaved ||
    metaWebsite !== node.website
  );

  const hostnameDirty = !!node && hostnameInput.trim() !== node.hostname && hostnameInput.trim() !== "";
  const listenStackDirty = !!node && listenStack !== (node.listen_stack ?? "dual");
  const settingsDirty = hostnameDirty || ipsDirty || prDirty || ratioDirty || metaDirty || listenStackDirty;
  const settingsValid = prValid && ratioValid;

  const saveSettings = async () => {
    if (!node || !settingsValid) return;
    setSaving(true);
    try {
      const payload: Record<string, unknown> = {};
      if (hostnameDirty) payload.hostname = hostnameInput.trim();
      if (ipsDirty) payload.server_ips = trimmedIps;
      if (prDirty) {
        payload.port_range_start = prStartNum;
        payload.port_range_end = prEndNum;
      }
      if (ratioDirty) payload.traffic_ratio = ratioNum;
      if (metaDirty) {
        payload.expires_at = metaExpiresAt === "" ? null : new Date(metaExpiresAt + "T00:00:00").toISOString();
        payload.monthly_price = metaPrice === "" ? null : Number(metaPrice);
        payload.website = metaWebsite;
      }
      if (listenStackDirty) payload.listen_stack = listenStack;
      if (Object.keys(payload).length > 0) {
        const updated = await Api.updateNode(node.id, payload as any);
        mutateNode(updated, { revalidate: false });
      }
      toast.success("设置已保存");
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  const rotateToken = async () => {
    try {
      const resp = await Api.rotateNodeToken(id);
      setRotated(resp);
      mutateNode();
      generateSetup(resp.id);
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e));
    }
  };



  const deleteNode = async () => {
    setConfirmDelete(false);
    try {
      await Api.deleteNode(id!);
      toast.success("节点已删除");
      navigate("/nodes");
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e));
    }
  };


  if (!node) {
    return <div className="text-sm text-muted-foreground">加载中…</div>;
  }

  const cpu = (series?.heartbeats ?? []).map((h) => h.cpu_pct);
  const mem = (series?.heartbeats ?? []).map((h) =>
    h.mem_total_bytes > 0 ? (h.mem_used_bytes / h.mem_total_bytes) * 100 : 0,
  );
  const conns = (series?.heartbeats ?? []).map((h) => h.active_connections);

  // 节点整体网络吞吐：按索引对各 tunnel 速率求和
  // 不同规则的样本长度可能不同，按从末尾对齐（最新样本按时钟对齐）
  const tunnelRates = Object.values(series?.tunnels ?? {}).map((s) => rates(s));
  const maxLen = Math.max(0, ...tunnelRates.map((r) => r.in.length));
  const netIn: number[] = [];
  const netOut: number[] = [];
  for (let i = 0; i < maxLen; i++) {
    let inSum = 0;
    let outSum = 0;
    for (const r of tunnelRates) {
      const off = r.in.length - maxLen + i;
      if (off >= 0) {
        inSum += r.in[off];
        outSum += r.out[off];
      }
    }
    netIn.push(inSum);
    netOut.push(outSum);
  }

  const last = <T,>(a: T[]): T | undefined => a[a.length - 1];
  const cpuNow = last(cpu);
  const memNow = last(mem);
  const connsNow = last(conns);
  const netInNow = last(netIn);
  const netOutNow = last(netOut);

  // TCP/UDP 连接分类 + 总流量，从 tunnel series 末帧按 forward 协议聚合
  let tcpConns = 0, udpConns = 0, totalBytesIn = 0, totalBytesOut = 0;
  if (series) {
    for (const f of forwards) {
      for (const p of f.ports.filter((p) => p.node_id === id)) {
        const key = `${f.id}:${p.hop_index}`;
        const lastSample = (series.tunnels[key] ?? []).at(-1);
        if (lastSample) {
          totalBytesIn += lastSample.bytes_in;
          totalBytesOut += lastSample.bytes_out;
          // forward 可同时支持 TCP+UDP，按协议集合分别计入
          const protos = f.protocols ?? [];
          if (protos.includes("tcp")) tcpConns += lastSample.active_connections;
          if (protos.includes("udp")) udpConns += lastSample.active_connections;
        }
      }
    }
  }
  const hasProtoStats = forwards.length > 0 && series != null;

  const fmtRate = (n: number) => `${fmtBytes(n)}/s`;

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-3">
        <Button asChild variant="ghost" size="sm">
          <Link to="/nodes">
            <ArrowLeft className="mr-2 h-4 w-4" /> 返回
          </Link>
        </Button>
      </div>

      <div className="flex items-end justify-between gap-3">
        <div>
          <div className="flex items-center gap-3">
            <h1 className="text-2xl font-semibold">{node.hostname || node.id}</h1>
            {isOnline(node) ? (
              <Badge variant="success">在线</Badge>
            ) : (
              <Badge variant="outline">离线</Badge>
            )}
          </div>
          <p className="text-sm text-muted-foreground">
            {node.version || "—"} · 最近上报{" "}
            {node.last_seen_at
              ? `${new Date(node.last_seen_at).toLocaleString()}（${timeAgo(node.last_seen_at)}）`
              : "从未"}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button
            size="sm"
            onClick={() => setUpgradeOpen(true)}
            disabled={!isOnline(node) || !node.capabilities?.includes("upgrade_v1")}
            title={
              !isOnline(node)
                ? "节点离线"
                : !node.capabilities?.includes("upgrade_v1")
                  ? "节点版本过低，请先手动升级到 0.2.x"
                  : "远程升级"
            }
          >
            <ArrowUpCircle className="mr-2 h-4 w-4" /> 升级
          </Button>
          <Button variant="outline" size="sm" onClick={rotateToken}>
            <RefreshCw className="mr-2 h-4 w-4" /> 重新安装
          </Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="icon" className="h-8 w-8">
                <MoreHorizontal className="h-4 w-4" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onClick={() => setConfirmDelete(true)} className="text-destructive">
                <Trash2 className="mr-2 h-4 w-4" /> 删除节点
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="text-sm">设置</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          {/* 名称 + 端口范围 + 流量倍率：紧凑一行 */}
          <div className="flex flex-wrap items-end gap-3">
            <div className="space-y-1 flex-1 min-w-40">
              <Label htmlFor="node-hostname" className="text-sm font-medium text-muted-foreground">名称</Label>
              <Input
                id="node-hostname"
                value={hostnameInput}
                onChange={(e) => setHostnameInput(e.target.value)}
                placeholder={node?.id ?? "节点名称"}
              />
            </div>
            <div className="space-y-1">
              <Label htmlFor="pr-start" className="text-sm font-medium text-muted-foreground">端口起始</Label>
              <Input
                id="pr-start"
                type="number"
                min={1}
                max={65535}
                className="w-28"
                value={prStart}
                onChange={(e) => {
                  const v = e.target.value;
                  setPrStart(v === "" ? "" : parseInt(v, 10));
                }}
              />
            </div>
            <div className="space-y-1">
              <Label htmlFor="pr-end" className="text-sm font-medium text-muted-foreground">端口结束</Label>
              <Input
                id="pr-end"
                type="number"
                min={1}
                max={65535}
                className="w-28"
                value={prEnd}
                onChange={(e) => {
                  const v = e.target.value;
                  setPrEnd(v === "" ? "" : parseInt(v, 10));
                }}
              />
            </div>
            <div className="space-y-1">
              <Label htmlFor="traffic-ratio" className="text-sm font-medium text-muted-foreground">流量倍率</Label>
              <Input
                id="traffic-ratio"
                type="number"
                min={0}
                step={0.1}
                className="w-24"
                value={ratioInput}
                onChange={(e) => setRatioInput(e.target.value)}
              />
            </div>
            <div className="space-y-1.5">
              <Label>监听协议栈</Label>
              <Select value={listenStack} onValueChange={setListenStack}>
                <SelectTrigger className="w-40">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="dual">双栈（默认）</SelectItem>
                  <SelectItem value="v4">仅 IPv4</SelectItem>
                  <SelectItem value="v6">仅 IPv6</SelectItem>
                </SelectContent>
              </Select>
              <p className="text-xs text-muted-foreground">控制该节点所有监听端口绑定的协议栈</p>
            </div>
          </div>
          {(!prValid || !ratioValid) && (
            <div className="space-y-1">
              {!prValid && <p className="text-sm text-destructive">端口范围无效：要求 1 ≤ 起始 ≤ 结束 ≤ 65535</p>}
              {!ratioValid && <p className="text-sm text-destructive">倍率必须为 ≥ 0 的数字</p>}
            </div>
          )}

          <section className="space-y-2 border-t pt-4">
            <div className="text-sm font-medium text-muted-foreground">公网 IP</div>
            {ipsInput.length === 0 ? (
              <div className="text-sm text-muted-foreground">
                尚未配置 — 节点下次连接时会自动填入源 IP。
              </div>
            ) : (
              ipsInput.map((value, i) => (
                <div key={i} className="flex items-center gap-2">
                  <div className="w-10 shrink-0">
                    {i === 0 ? (
                      <Badge variant="default">主</Badge>
                    ) : (
                      <Badge variant="outline">备</Badge>
                    )}
                  </div>
                  <Input
                    value={value}
                    onChange={(e) => updateIp(i, e.target.value)}
                    placeholder="例如 203.0.113.10"
                    className="flex-1"
                  />
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="h-8 w-8"
                    onClick={() => moveIp(i, -1)}
                    disabled={i === 0}
                    title="上移"
                  >
                    <ArrowUp className="h-4 w-4" />
                  </Button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="h-8 w-8"
                    onClick={() => moveIp(i, 1)}
                    disabled={i === ipsInput.length - 1}
                    title="下移"
                  >
                    <ArrowDown className="h-4 w-4" />
                  </Button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="h-8 w-8"
                    onClick={() => removeIp(i)}
                    title="删除"
                  >
                    <X className="h-4 w-4" />
                  </Button>
                </div>
              ))
            )}
            <div className="flex items-center gap-2 pt-1">
              <Button type="button" variant="outline" size="sm" onClick={addIp}>
                <Plus className="mr-1 h-4 w-4" /> 添加
              </Button>
            </div>
          </section>

          <section className="space-y-2 border-t pt-4">
            <div className="text-sm font-medium text-muted-foreground">节点信息</div>
            <div className="grid gap-4 sm:grid-cols-3">
              <div className="space-y-1">
                <Label htmlFor="node-expires" className="text-sm">到期时间</Label>
                <Input
                  id="node-expires"
                  type="date"
                  value={metaExpiresAt}
                  onChange={(e) => setMetaExpiresAt(e.target.value)}
                />
              </div>
              <div className="space-y-1">
                <Label htmlFor="node-price" className="text-sm">月均价格</Label>
                <Input
                  id="node-price"
                  type="number"
                  min={0}
                  step={0.01}
                  placeholder="0.00"
                  value={metaPrice}
                  onChange={(e) => setMetaPrice(e.target.value)}
                />
              </div>
              <div className="space-y-1">
                <Label htmlFor="node-website" className="text-sm">官网</Label>
                <Input
                  id="node-website"
                  type="url"
                  placeholder="https://"
                  value={metaWebsite}
                  onChange={(e) => setMetaWebsite(e.target.value)}
                />
              </div>
            </div>
          </section>

          <div className="border-t pt-4 flex justify-end">
            <Button
              onClick={saveSettings}
              disabled={!settingsDirty || !settingsValid || saving}
            >
              <Save className="mr-2 h-4 w-4" />
              {saving ? "保存中…" : "保存"}
            </Button>
          </div>
        </CardContent>
      </Card>

      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
        <Card>
          <CardHeader>
            <CardTitle className="text-sm flex items-baseline justify-between">
              <span>CPU %</span>
              <span className="text-base font-semibold tabular-nums">
                {cpuNow !== undefined ? `${cpuNow.toFixed(1)}%` : "—"}
              </span>
            </CardTitle>
            <CardDescription>最近 {cpu.length} 个采样</CardDescription>
          </CardHeader>
          <CardContent>
            <Sparkline data={cpu} format={(v) => `${v.toFixed(1)}%`} />
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="text-sm flex items-baseline justify-between">
              <span>内存 %</span>
              <span className="text-base font-semibold tabular-nums">
                {memNow !== undefined ? `${memNow.toFixed(1)}%` : "—"}
              </span>
            </CardTitle>
            <CardDescription>已用 / 总量</CardDescription>
          </CardHeader>
          <CardContent>
            <Sparkline data={mem} format={(v) => `${v.toFixed(1)}%`} />
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="text-sm flex items-baseline justify-between">
              <span>活动连接</span>
              <span className="text-base font-semibold tabular-nums">
                {connsNow !== undefined ? connsNow : "—"}
              </span>
            </CardTitle>
            <CardDescription>
              {hasProtoStats ? `TCP ${tcpConns} · UDP ${udpConns}` : "整节点"}
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Sparkline data={conns} />
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="text-sm flex items-baseline justify-between">
              <span>网络</span>
              <span className="text-xs font-medium tabular-nums">
                ↓ {netInNow !== undefined ? fmtRate(netInNow) : "—"}
                <span className="mx-1 text-muted-foreground">·</span>
                ↑ {netOutNow !== undefined ? fmtRate(netOutNow) : "—"}
              </span>
            </CardTitle>
            <CardDescription>
              {hasProtoStats
                ? `累计 ${fmtBytes(totalBytesIn + totalBytesOut)}（入 ${fmtBytes(totalBytesIn)} / 出 ${fmtBytes(totalBytesOut)}）`
                : "整节点入向 / 出向"}
            </CardDescription>
          </CardHeader>
          <CardContent>
            <div className="space-y-2">
              <div>
                <div className="text-[10px] text-muted-foreground">入</div>
                <Sparkline data={netIn} format={fmtRate} />
              </div>
              <div>
                <div className="text-[10px] text-muted-foreground">出</div>
                <Sparkline data={netOut} format={fmtRate} />
              </div>
            </div>
          </CardContent>
        </Card>
      </div>

      {(() => {
        if (!node.cert_not_after) return null;
        const ms = new Date(node.cert_not_after).getTime() - Date.now();
        const days = Math.floor(ms / 86400_000);
        if (days > 14) return null;
        const expired = ms <= 0;
        return (
          <div
            className={`rounded-md border px-4 py-3 text-sm ${
              expired
                ? "border-destructive/40 bg-destructive/10 text-destructive"
                : "border-amber-500/40 bg-amber-500/10 text-amber-700 dark:text-amber-300"
            }`}
          >
            {expired ? (
              <>
                <strong>节点证书已过期。</strong> 节点已无法连上 master，需要点
                「重新安装」生成新令牌后到节点机器上重跑安装命令。
              </>
            ) : (
              <>
                <strong>节点证书将在 {days} 天后过期。</strong> agent
                在线时会自动续期；如长期离线，请提前点「重新安装」。
              </>
            )}
          </div>
        );
      })()}


      <Dialog open={confirmDelete} onOpenChange={setConfirmDelete}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>确认删除节点</DialogTitle>
            <DialogDescription>
              此操作不可撤销，节点及其所有配置将被永久删除。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setConfirmDelete(false)}>取消</Button>
            <Button variant="destructive" onClick={deleteNode}>确认删除</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <UpgradeNodeDialog
        open={upgradeOpen}
        onOpenChange={setUpgradeOpen}
        node={node}
        onSuccess={() => mutateNode()}
      />

      <Dialog open={!!rotated} onOpenChange={(o) => !o && setRotated(null)}>
        <DialogContent className="sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>重新安装节点 — 安装命令</DialogTitle>
          </DialogHeader>
          {rotated && (
            <div>
              <Label className="text-sm uppercase text-muted-foreground">安装命令</Label>
              <div className="mt-2 mb-2">
                <Input
                  placeholder="GitHub 镜像前缀（可选，如 https://gh-proxy.com/）"
                  value={mirrorUrl}
                  onChange={(e) => setMirrorUrl(e.target.value)}
                  className="h-8 text-xs"
                />
              </div>
              <pre className="whitespace-pre-wrap break-all rounded-md border bg-muted p-3 text-xs">
                <ShellCmd setupUrl={setupUrl} mirrorUrl={mirrorUrl} />
              </pre>
              <Button variant="outline" size="sm" className="mt-2" onClick={copyReenrollCmd}>
                {cmdCopied ? <Check className="mr-2 h-4 w-4" /> : <Copy className="mr-2 h-4 w-4" />}
                {cmdCopied ? "已复制" : "复制命令"}
              </Button>
            </div>
          )}
          <DialogFooter>
            <Button onClick={() => setRotated(null)}>完成</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Card>
        <CardHeader>
          <CardTitle>转发 / 端口占用</CardTitle>
        </CardHeader>
        <CardContent className="p-0">
          <ScrollArea>
          <Table className="min-w-[880px] table-fixed">
            <colgroup>
              <col style={{ width: "10rem" }} />
              <col style={{ width: "5rem" }} />
              <col style={{ width: "5rem" }} />
              <col style={{ width: "6rem" }} />
              <col style={{ width: "6rem" }} />
              <col />
              <col style={{ width: "8rem" }} />
              <col style={{ width: "8rem" }} />
            </colgroup>
            <TableHeader>
              <TableRow>
                <TableHead>名称</TableHead>
                <TableHead>角色</TableHead>
                <TableHead>端口</TableHead>
                <TableHead>活动连接</TableHead>
                <TableHead>累计连接</TableHead>
                <TableHead>入 / 出 字节</TableHead>
                <TableHead>入向速率（B/s）</TableHead>
                <TableHead>出向速率（B/s）</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {forwards.length === 0 ? (
                <TableRow>
                  <TableCell colSpan={8} className="text-center text-muted-foreground">
                    该节点暂无转发。
                  </TableCell>
                </TableRow>
              ) : (
                forwards.flatMap((f) => {
                  const lastIdx = f.ports.reduce((m, p) => Math.max(m, p.hop_index), 0);
                  // 当前节点参与的层（dedupe by hop_index：同一层多协议共享 listen_port
                  // 且 stats 按 hop_index 聚合，没必要分行；分层 DAG 同节点在同一
                  // tunnel 也只属于唯一一层，所以 hop_index 即可作为 key）。
                  const seen = new Set<number>();
                  const layerPorts = f.ports
                    .filter((p) => p.node_id === id)
                    .filter((p) => {
                      if (seen.has(p.hop_index)) return false;
                      seen.add(p.hop_index);
                      return true;
                    });
                  return layerPorts.map((p) => {
                      const role =
                        p.hop_index === 0 && lastIdx === 0
                          ? "单跳"
                          : p.hop_index === 0
                            ? "入口"
                            : p.hop_index === lastIdx
                              ? "出口"
                              : "中转";
                      const statsKey = `${f.id}:${p.hop_index}`;
                      const samples = series?.tunnels[statsKey] ?? [];
                      const last = samples[samples.length - 1];
                      const r = rates(samples);
                      return (
                        <TableRow key={`${f.id}:${p.hop_index}`}>
                          <TableCell className="font-medium truncate" title={f.name}>{f.name}</TableCell>
                          <TableCell>
                            <Badge variant="secondary">{role}</Badge>
                          </TableCell>
                          <TableCell className="font-mono text-xs whitespace-nowrap">{p.listen_port}</TableCell>
                          <TableCell>{last?.active_connections ?? 0}</TableCell>
                          <TableCell>{last?.total_connections ?? 0}</TableCell>
                          <TableCell className="text-sm whitespace-nowrap">
                            {fmtBytes(last?.bytes_in ?? 0)} / {fmtBytes(last?.bytes_out ?? 0)}
                          </TableCell>
                          <TableCell>
                            <Sparkline data={r.in} width={120} height={32} format={fmtBytes} />
                          </TableCell>
                          <TableCell>
                            <Sparkline data={r.out} width={120} height={32} format={fmtBytes} />
                          </TableCell>
                        </TableRow>
                      );
                    });
                })
              )}
            </TableBody>
          </Table>
          </ScrollArea>
        </CardContent>
      </Card>
    </div>
  );
}
