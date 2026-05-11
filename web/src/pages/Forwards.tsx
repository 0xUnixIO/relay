import { useEffect, useMemo, useState } from "react";
import { useForwardStats } from "@/hooks/useForwardStats";
import { useConfirm } from "@/hooks/useConfirm";
import useSWR from "swr";
import {
  Plus, Pencil, Trash2, Pause, Play, RefreshCw, Activity, Shuffle, Check, Network, MoreHorizontal, BarChart2, LineChart,
} from "lucide-react";
import { Card, CardContent } from "@/components/ui/card";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { EmptyState } from "@/components/ui/empty-state";
import {
  Table, TableBody, TableCell, TableHead, TableHeader, TableRow,
} from "@/components/ui/table";
import {
  Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
} from "@/components/ui/dialog";
import {
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from "@/components/ui/select";
import {
  DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Api, type Forward, type Tunnel, type NodeInfo, type ForwardStatBucket,
} from "@/lib/api";
import { getRole } from "@/lib/auth";
import { toast } from "sonner";

function fmtBytes(n: number): string {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let v = n, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return `${v.toFixed(v < 10 ? 1 : 0)} ${u[i]}`;
}

const LB_LABELS: Record<string, string> = {
  round_robin: "轮询",
  random: "随机",
  least_latency: "最低延迟",
  primary_backup: "主备",
};

function parseUpstreamAddrs(text: string): string[] | string {
  const lines = text.split(/[\n,]/).map((l) => l.trim()).filter(Boolean);
  if (lines.length === 0) return "上游地址不能为空";
  const re = /^[^\s]+:\d{1,5}$/;
  for (const line of lines) {
    if (!re.test(line)) return `无效地址「${line}」，格式须为 host:port`;
    const port = Number(line.split(":").at(-1));
    if (port < 1 || port > 65535) return `地址「${line}」端口须在 1-65535 之间`;
  }
  return lines;
}

type Form = {
  tunnel_id: string;
  name: string;
  in_port: string;
  remote_addrs: string;
  lb_strategy: string;
};

const emptyForm = (): Form => ({
  tunnel_id: "",
  name: "",
  in_port: "",
  remote_addrs: "",
  lb_strategy: "round_robin",
});

type ProbeState =
  | { status: "idle" }
  | { status: "checking" }
  | { status: "free" }
  | { status: "busy"; error: string }
  | { status: "error"; error: string };

export default function ForwardsPage() {
  const confirm = useConfirm();
  const isAdmin = getRole() === "admin";
  const { data: forwards = [], mutate } = useSWR("forwards", Api.listForwards);
  // 所有用户都能获取隧道列表（非 admin 只返回 enabled 的）
  const { data: tunnels = [] } = useSWR<Tunnel[]>("tunnels", Api.listTunnels);
  const { data: nodes = [] } = useSWR<NodeInfo[]>(
    isAdmin ? "nodes" : null,
    isAdmin ? Api.listNodes : null,
  );

  const [open, setOpen] = useState(false);
  const [editing, setEditing] = useState<Forward | null>(null);
  const [form, setForm] = useState<Form>(emptyForm());
  const [saving, setSaving] = useState(false);
  const [hostProbe, setHostProbe] = useState<ProbeState>({ status: "idle" });

  const rates = useForwardStats();

  const [probing, setProbing] = useState<Record<string, boolean>>({});
  const [probeResult, setProbeResult] = useState<{
    forward: Forward;
    hops: import("@/lib/api").ForwardProbeHop[];
  } | null>(null);
  const [statsTarget, setStatsTarget] = useState<Forward | null>(null);
  const [probeStatsTarget, setProbeStatsTarget] = useState<Forward | null>(null);
  const [copied, setCopied] = useState<string | null>(null);

  const copy = (key: string, text: string) => {
    navigator.clipboard.writeText(text);
    setCopied(key);
    toast.success("已复制");
    setTimeout(() => setCopied(null), 1500);
  };

  const tunnelById = useMemo(() => {
    const m = new Map<string, Tunnel>();
    for (const t of tunnels) m.set(t.id, t);
    return m;
  }, [tunnels]);

  // 当前表单选中的入口节点（admin only，用于端口探测）
  const entryNodeId = useMemo(() => {
    if (!isAdmin || !form.tunnel_id) return null;
    const tunnel = tunnelById.get(form.tunnel_id);
    if (!tunnel) return null;
    const sorted = (tunnel.hops ?? []).slice().sort((a, b) => a.hop_index - b.hop_index);
    return sorted[0]?.node_id ?? null;
  }, [isAdmin, form.tunnel_id, tunnelById]);

  const entryNode = useMemo(
    () => nodes.find((n) => n.id === entryNodeId) ?? null,
    [nodes, entryNodeId],
  );

  // 已被占用的入口端口（排除当前正在编辑的 forward）
  const usedPorts = useMemo(() => {
    const set = new Set<number>();
    for (const f of forwards) {
      if (editing && f.id === editing.id) continue;
      const entry = f.ports?.find((p) => p.hop_index === 0);
      if (entry?.listen_port) set.add(entry.listen_port);
    }
    return set;
  }, [forwards, editing]);

  const portNum = form.in_port.trim() ? Number(form.in_port) : NaN;
  const portValid = Number.isInteger(portNum) && portNum >= 1 && portNum <= 65535;
  const portConflict = portValid && usedPorts.has(portNum);

  // 宿主端口占用探测（admin + 有入口节点 + 端口有效且无冲突）
  useEffect(() => {
    if (!open || !entryNodeId || !portValid || portConflict) {
      setHostProbe({ status: "idle" });
      return;
    }
    setHostProbe({ status: "checking" });
    const handle = setTimeout(async () => {
      try {
        const res = await Api.probeNodePort(entryNodeId, portNum, "tcp");
        setHostProbe(res.free ? { status: "free" } : { status: "busy", error: res.error || "" });
      } catch (e: any) {
        setHostProbe({ status: "error", error: e?.message ?? "探测失败" });
      }
    }, 400);
    return () => clearTimeout(handle);
  }, [open, editing, entryNodeId, portNum, portValid, portConflict]);

  const randomPort = () => {
    const rangeStart = entryNode?.port_range_start || 10000;
    const rangeEnd = entryNode?.port_range_end || 60000;
    const span = rangeEnd - rangeStart + 1;
    if (span <= 0) { toast.error("节点端口范围无效"); return; }
    for (let i = 0; i < 200; i++) {
      const p = rangeStart + Math.floor(Math.random() * span);
      if (!usedPorts.has(p)) {
        setForm((f) => ({ ...f, in_port: String(p) }));
        return;
      }
    }
    toast.error("端口范围内无可用空闲端口");
  };

  const openNew = () => {
    setEditing(null);
    setForm(emptyForm());
    setHostProbe({ status: "idle" });
    setOpen(true);
  };
  const openEdit = (f: Forward) => {
    setEditing(f);
    setForm({
      tunnel_id: f.tunnel_id,
      name: f.name,
      in_port: String(f.in_port ?? ""),
      remote_addrs: (f.remote_addrs ?? []).join("\n"),
      lb_strategy: f.lb_strategy || "round_robin",
    });
    setHostProbe({ status: "idle" });
    setOpen(true);
  };

  const submit = async () => {
    const parsed = parseUpstreamAddrs(form.remote_addrs);
    if (typeof parsed === "string") { toast.error(parsed); return; }
    if (portConflict) { toast.error(`端口 ${portNum} 已被占用`); return; }
    const inPort = form.in_port.trim() ? Number(form.in_port) : undefined;
    setSaving(true);
    try {
      if (editing) {
        const inPort = form.in_port.trim() ? Number(form.in_port) : undefined;
        await Api.updateForward(editing.id, {
          name: form.name,
          in_port: inPort,
          remote_addrs: parsed,
          lb_strategy: form.lb_strategy || undefined,
        });
      } else {
        if (!form.tunnel_id) { toast.error("请选择隧道"); setSaving(false); return; }
        const created = await Api.createForward({
          tunnel_id: form.tunnel_id,
          name: form.name,
          in_port: inPort,
          remote_addrs: parsed,
          lb_strategy: form.lb_strategy || undefined,
        });
        if (created.port_warnings?.length) {
          for (const w of created.port_warnings) toast.warning(w);
        }
      }
      setOpen(false);
      mutate();
      toast.success(editing ? "已更新" : "已创建");
    } catch (e: any) {
      toast.error(e?.message ?? "保存失败");
    } finally {
      setSaving(false);
    }
  };

  const remove = async (f: Forward) => {
    if (!await confirm(`删除转发 "${f.name}"？端口将被释放。`)) return;
    try {
      await Api.deleteForward(f.id);
      mutate();
    } catch (e: any) {
      toast.error(e?.message ?? "删除失败");
    }
  };

  const toggle = async (f: Forward) => {
    try {
      if (f.desired_enabled) await Api.pauseForward(f.id);
      else await Api.resumeForward(f.id);
      mutate();
    } catch (e: any) {
      toast.error(e?.message ?? "操作失败");
    }
  };

  const redeploy = async (f: Forward) => {
    try {
      await Api.redeployForward(f.id);
      toast.success("已触发重新部署");
      mutate();
    } catch (e: any) {
      toast.error(e?.message ?? "重新部署失败");
    }
  };

  const probe = async (f: Forward) => {
    setProbing((p) => ({ ...p, [f.id]: true }));
    try {
      const hops = await Api.probeForward(f.id);
      setProbeResult({ forward: f, hops });
    } catch (e: any) {
      toast.error(e?.message ?? "探测失败");
    } finally {
      setProbing((p) => ({ ...p, [f.id]: false }));
    }
  };

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold">转发</h1>
        <Button onClick={openNew} disabled={tunnels.length === 0}>
          <Plus className="mr-1 h-4 w-4" /> 新建转发
        </Button>
      </div>

      {tunnels.length === 0 && (
        <Card>
          <CardContent className="py-6 text-sm text-muted-foreground">
            暂无可用隧道，请先在「隧道」页创建并启用隧道。
          </CardContent>
        </Card>
      )}

      {/* overflow-x-auto 防止窄屏下表格撑破页面布局 */}
      <Card>
        <CardContent className="p-0 overflow-x-auto">
          <Table className="w-full">
            <TableHeader>
              <TableRow>
                <TableHead>名称</TableHead>
                <TableHead className="w-[9rem]">入口</TableHead>
                {/* 上游/下载/上传/活跃连接列在移动端隐藏，优先保证名称和操作可见 */}
                <TableHead className="hidden sm:table-cell w-[12rem]">上游</TableHead>
                <TableHead className="hidden sm:table-cell w-px whitespace-nowrap text-right">下载</TableHead>
                <TableHead className="hidden sm:table-cell w-px whitespace-nowrap text-right">上传</TableHead>
                <TableHead className="hidden sm:table-cell w-px whitespace-nowrap text-right">活跃连接</TableHead>
                <TableHead className="w-px whitespace-nowrap text-right">操作</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {forwards.length === 0 ? (
                <TableRow className="hover:bg-transparent even:bg-transparent">
                  {/* 移动端 3 列（名称、入口、操作），sm 以上 7 列 */}
                  <TableCell colSpan={3} className="text-center sm:hidden">
                    <EmptyState icon={Network} title="暂无转发" description="点击右上角「新建转发」按钮创建。" compact />
                  </TableCell>
                  <TableCell colSpan={7} className="text-center hidden sm:table-cell">
                    <EmptyState icon={Network} title="暂无转发" description="点击右上角「新建转发」按钮创建。" compact />
                  </TableCell>
                </TableRow>
              ) : (
                [...forwards].sort((a, b) => Number(b.effective_enabled) - Number(a.effective_enabled)).map((f) => {
                  const hasDeployError = f.pause_reasons.includes("deploy_failed");
                  const rowCls = f.effective_enabled
                    ? ""
                    : hasDeployError
                      ? "!bg-amber-100 dark:!bg-amber-900/50 text-muted-foreground"
                      : "!bg-gray-200 dark:!bg-gray-700/60 text-muted-foreground";
                  return (
                    <TableRow key={f.id} className={rowCls}>
                      <TableCell className="min-w-0">
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <div>
                              <div className="font-medium truncate">{f.name}</div>
                              <div className="text-xs text-muted-foreground truncate">{f.tunnel_name}</div>
                            </div>
                          </TooltipTrigger>
                          <TooltipContent>{f.name} · {f.tunnel_name}</TooltipContent>
                        </Tooltip>
                      </TableCell>
                      <TableCell className="font-mono text-xs text-muted-foreground">
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <span
                              className="inline-flex items-center gap-1 min-w-0 max-w-full cursor-pointer hover:text-foreground"
                              onClick={() => copy(
                                `${f.id}-port`,
                                f.entry_addrs && f.entry_addrs.length > 1
                                  ? f.entry_addrs.join("\n")
                                  : f.entry_addr ?? String(f.in_port),
                              )}
                            >
                              <span className="truncate">{f.entry_addr ?? String(f.in_port)}</span>
                              {f.entry_addrs && f.entry_addrs.length > 1 && (
                                <Badge variant="secondary" className="shrink-0">×{f.entry_addrs.length}</Badge>
                              )}
                              <Check className={`h-3 w-3 shrink-0 ${copied === `${f.id}-port` ? "text-emerald-500" : "invisible"}`} />
                            </span>
                          </TooltipTrigger>
                          <TooltipContent>
                            {f.entry_addrs && f.entry_addrs.length > 1
                              ? f.entry_addrs.join(" / ")
                              : (f.entry_addr ?? String(f.in_port))}
                          </TooltipContent>
                        </Tooltip>
                      </TableCell>
                      {/* 上游列 — 移动端隐藏 */}
                      <TableCell className="hidden sm:table-cell font-mono text-xs text-muted-foreground">
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <span
                              className="inline-flex items-center gap-1 min-w-0 max-w-full cursor-pointer hover:text-foreground"
                              onClick={() => copy(`${f.id}-upstream`, f.remote_addrs.join("\n"))}
                            >
                              <span className="truncate min-w-0">{f.remote_addrs.slice(0, 1).join(", ")}</span>
                              {f.remote_addrs.length > 1 && (
                                <>
                                  <Badge variant="secondary" className="shrink-0">+{f.remote_addrs.length - 1}</Badge>
                                  <span className="shrink-0 text-muted-foreground">
                                    {LB_LABELS[f.lb_strategy] ?? f.lb_strategy}
                                  </span>
                                </>
                              )}
                              <Check className={`h-3 w-3 shrink-0 ${copied === `${f.id}-upstream` ? "text-emerald-500" : "invisible"}`} />
                            </span>
                          </TooltipTrigger>
                          <TooltipContent>
                            <div className="font-mono text-xs">
                              {f.remote_addrs.map((a, i) => <div key={i}>{a}</div>)}
                            </div>
                          </TooltipContent>
                        </Tooltip>
                      </TableCell>
                      {/* 下载列 — 移动端隐藏 */}
                      <TableCell className="hidden sm:table-cell text-right font-mono text-xs whitespace-nowrap">
                        <div>{fmtBytes(f.in_flow_bytes)}</div>
                        {rates.get(f.id)?.inRate != null && rates.get(f.id)!.inRate > 0 && (
                          <div className="text-emerald-500">{fmtBytes(rates.get(f.id)!.inRate)}/s</div>
                        )}
                      </TableCell>
                      {/* 上传列 — 移动端隐藏 */}
                      <TableCell className="hidden sm:table-cell text-right font-mono text-xs whitespace-nowrap">
                        <div>{fmtBytes(f.out_flow_bytes)}</div>
                        {rates.get(f.id)?.outRate != null && rates.get(f.id)!.outRate > 0 && (
                          <div className="text-sky-500">{fmtBytes(rates.get(f.id)!.outRate)}/s</div>
                        )}
                      </TableCell>
                      {/* 活跃连接列 — 移动端隐藏 */}
                      <TableCell className="hidden sm:table-cell text-right font-mono text-xs whitespace-nowrap">{f.active_connections}</TableCell>
                      {/* 操作列：移动端只显示 DropdownMenu（探测和编辑合并入菜单），sm 以上保持三按钮并排 */}
                      <TableCell className="whitespace-nowrap text-right">
                        <div className="inline-flex items-center gap-1">
                          {/* 探测按钮 — 仅 sm 以上显示 */}
                          <Tooltip>
                            <TooltipTrigger asChild>
                              <Button
                                size="icon"
                                variant="ghost"
                                className="hidden sm:inline-flex"
                                onClick={() => probe(f)}
                                disabled={!!probing[f.id]}
                              >
                                <Activity className={`h-4 w-4 ${probing[f.id] ? "animate-pulse" : ""}`} />
                              </Button>
                            </TooltipTrigger>
                            <TooltipContent>探测上游延迟</TooltipContent>
                          </Tooltip>
                          {/* 编辑按钮 — 仅 sm 以上显示 */}
                          <Tooltip>
                            <TooltipTrigger asChild>
                              <Button
                                size="icon"
                                variant="ghost"
                                className="hidden sm:inline-flex"
                                onClick={() => openEdit(f)}
                              >
                                <Pencil className="h-4 w-4" />
                              </Button>
                            </TooltipTrigger>
                            <TooltipContent>编辑</TooltipContent>
                          </Tooltip>
                          <DropdownMenu>
                            <DropdownMenuTrigger asChild>
                              <Button size="icon" variant="ghost">
                                <MoreHorizontal className="h-4 w-4" />
                              </Button>
                            </DropdownMenuTrigger>
                            <DropdownMenuContent align="end">
                              {/* 移动端额外提供探测和编辑入口 */}
                              <DropdownMenuItem className="sm:hidden" onClick={() => probe(f)} disabled={!!probing[f.id]}>
                                <Activity className={`h-4 w-4 mr-2 ${probing[f.id] ? "animate-pulse" : ""}`} />
                                {probing[f.id] ? "探测中…" : "探测延迟"}
                              </DropdownMenuItem>
                              <DropdownMenuItem className="sm:hidden" onClick={() => openEdit(f)}>
                                <Pencil className="h-4 w-4 mr-2" />编辑
                              </DropdownMenuItem>
                              <DropdownMenuSeparator className="sm:hidden" />
                              <DropdownMenuItem onClick={() => setStatsTarget(f)}>
                                <BarChart2 className="h-4 w-4 mr-2" />流量趋势
                              </DropdownMenuItem>
                              <DropdownMenuItem onClick={() => setProbeStatsTarget(f)}>
                                <LineChart className="h-4 w-4 mr-2" />延迟统计
                              </DropdownMenuItem>
                              <DropdownMenuSeparator />
                              <DropdownMenuItem onClick={() => toggle(f)}>
                                {f.desired_enabled
                                  ? <><Pause className="h-4 w-4 mr-2" />暂停</>
                                  : <><Play className="h-4 w-4 mr-2" />恢复</>}
                              </DropdownMenuItem>
                              <DropdownMenuItem onClick={() => redeploy(f)}>
                                <RefreshCw className="h-4 w-4 mr-2" />重新部署
                              </DropdownMenuItem>
                              <DropdownMenuSeparator />
                              <DropdownMenuItem
                                className="text-destructive focus:text-destructive"
                                onClick={() => remove(f)}
                              >
                                <Trash2 className="h-4 w-4 mr-2" />删除
                              </DropdownMenuItem>
                            </DropdownMenuContent>
                          </DropdownMenu>
                        </div>
                      </TableCell>
                    </TableRow>
                  );
                })
              )}
            </TableBody>
          </Table>
        </CardContent>
      </Card>

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="sm:max-w-xl">
          <DialogHeader>
            <DialogTitle>{editing ? "编辑转发" : "新建转发"}</DialogTitle>
            <DialogDescription>
              转发依附于已分配给用户的隧道；上游地址在出口节点解析。
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-4">
            {!editing && (
              <div className="space-y-1.5">
                <Label>隧道</Label>
                <Select
                  value={form.tunnel_id}
                  onValueChange={(v) => setForm({ ...form, tunnel_id: v })}
                >
                  <SelectTrigger>
                    <SelectValue placeholder="选择隧道…" />
                  </SelectTrigger>
                  <SelectContent>
                    {tunnels.map((t) => (
                      <SelectItem key={t.id} value={t.id}>
                        {t.name} · {(t.protocols ?? []).map((p) => p.toUpperCase()).join("+")}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            )}
            {editing && (
              <div className="text-xs text-muted-foreground">
                隧道：{editing.tunnel_name}
              </div>
            )}
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
              <div className="space-y-1.5">
                <Label>名称</Label>
                <Input
                  value={form.name}
                  onChange={(e) => setForm({ ...form, name: e.target.value })}
                />
              </div>
              <div className="space-y-1.5">
                <Label>入口端口（留空自动分配）</Label>
                <div className="flex gap-2">
                  <Input
                    type="number"
                    value={form.in_port}
                    onChange={(e) => setForm({ ...form, in_port: e.target.value })}
                    aria-invalid={form.in_port !== "" && (!portValid || portConflict)}
                    className="flex-1"
                  />
                  <Tooltip>
                    <TooltipTrigger asChild>
                      <span tabIndex={!entryNodeId ? 0 : undefined}>
                        <Button type="button" variant="outline" size="icon" onClick={randomPort} disabled={!entryNodeId}>
                          <Shuffle className="h-4 w-4" />
                        </Button>
                      </span>
                    </TooltipTrigger>
                    <TooltipContent>
                      {!entryNodeId ? "请先选择隧道" : entryNode ? `随机空闲端口（${entryNode.port_range_start}–${entryNode.port_range_end}）` : "随机分配端口"}
                    </TooltipContent>
                  </Tooltip>
                </div>
                {form.in_port !== "" && (
                  <p className="text-xs">
                    {!portValid && <span className="text-destructive">端口须在 1-65535 之间</span>}
                    {portConflict && <span className="text-destructive">端口 {portNum} 已被其他转发占用</span>}
                    {!portConflict && portValid && (
                      <>
                        {hostProbe.status === "checking" && <span className="text-muted-foreground">检测宿主占用中…</span>}
                        {hostProbe.status === "free" && <span className="text-emerald-600 dark:text-emerald-400">✓ 节点 {entryNodeId} 上端口 {portNum} 空闲</span>}
                        {hostProbe.status === "busy" && <span className="text-destructive">✗ 端口 {portNum} 已被节点宿主进程占用{hostProbe.error ? `（${hostProbe.error}）` : ""}</span>}
                        {hostProbe.status === "error" && <span className="text-muted-foreground">无法探测：{hostProbe.error}</span>}
                      </>
                    )}
                  </p>
                )}
              </div>
            </div>
            <div className="space-y-1.5">
              <Label>上游地址（每行一条 host:port）</Label>
              <textarea
                className="w-full min-h-[80px] rounded-md border bg-background p-2 font-mono text-xs"
                value={form.remote_addrs}
                onChange={(e) => setForm({ ...form, remote_addrs: e.target.value })}
                placeholder="example.com:8080"
              />
            </div>
            {form.remote_addrs.trim().includes("\n") && (
              <div className="space-y-1.5">
                <Label>负载均衡策略</Label>
                <Select
                  value={form.lb_strategy}
                  onValueChange={(v) => setForm({ ...form, lb_strategy: v })}
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="round_robin">轮询</SelectItem>
                    <SelectItem value="random">随机</SelectItem>
                    <SelectItem value="least_latency">最低延迟</SelectItem>
                  </SelectContent>
                </Select>
              </div>
            )}
          </div>

          <DialogFooter>
            <Button variant="ghost" onClick={() => setOpen(false)} disabled={saving}>
              取消
            </Button>
            <Button onClick={submit} disabled={saving}>
              {saving ? "保存中…" : "保存"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 流量趋势 */}
      <Dialog open={!!statsTarget} onOpenChange={(v) => !v && setStatsTarget(null)}>
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <BarChart2 className="h-4 w-4" />
              流量趋势 · {statsTarget?.name}
            </DialogTitle>
          </DialogHeader>
          {statsTarget && <ForwardStatsChart forwardId={statsTarget.id} />}
          <DialogFooter>
            <Button variant="ghost" onClick={() => setStatsTarget(null)}>关闭</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 上游延迟统计 */}
      <Dialog open={!!probeStatsTarget} onOpenChange={(v) => !v && setProbeStatsTarget(null)}>
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <LineChart className="h-4 w-4" />
              上游延迟统计 · {probeStatsTarget?.name}
            </DialogTitle>
            <DialogDescription>最近 24 小时，每 5 分钟探测一次</DialogDescription>
          </DialogHeader>
          {probeStatsTarget && <UpstreamProbeStatsPanel forwardId={probeStatsTarget.id} />}
          <DialogFooter>
            <Button variant="ghost" onClick={() => setProbeStatsTarget(null)}>关闭</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 延迟探测结果 */}
      <Dialog open={!!probeResult} onOpenChange={(v) => !v && setProbeResult(null)}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <Activity className="h-4 w-4" />
              延迟探测
            </DialogTitle>
            <DialogDescription>
              {probeResult ? `「${probeResult.forward.name}」` : ""}
            </DialogDescription>
          </DialogHeader>
          {probeResult && (
            <div className="space-y-3">
              <ProbeTopology hops={probeResult.hops} />
              {probeResult.hops.some((h) => !h.ok) && (
                <div className="rounded-md border border-rose-200 bg-rose-50/60 p-2 text-xs text-rose-700 dark:border-rose-900/40 dark:bg-rose-950/30 dark:text-rose-400">
                  {probeResult.hops.filter((h) => !h.ok).map((h, i) => (
                    <div key={i} className="font-mono">{h.error}</div>
                  ))}
                </div>
              )}
            </div>
          )}
          <DialogFooter>
            <Button variant="ghost" onClick={() => setProbeResult(null)}>
              关闭
            </Button>
            {probeResult && (
              <Button
                onClick={() => probe(probeResult.forward)}
                disabled={!!probing[probeResult.forward.id]}
              >
                <Activity
                  className={`mr-1 h-4 w-4 ${probing[probeResult.forward.id] ? "animate-pulse" : ""}`}
                />
                {probing[probeResult.forward.id] ? "探测中…" : "重新探测"}
              </Button>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

import { ProbeTopology } from "@/components/ProbeTopology";

// ── 流量趋势图表 ──────────────────────────────────────────────────────────────

import {
  AreaChart, Area, XAxis, YAxis, Tooltip as ReTooltip,
  ResponsiveContainer, CartesianGrid,
} from "recharts";

type Period = "1h" | "24h" | "7d";

function ForwardStatsChart({ forwardId }: { forwardId: string }) {
  const [period, setPeriod] = useState<Period>("1h");
  const [data, setData] = useState<ForwardStatBucket[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    setLoading(true);
    Api.getForwardStats(forwardId, period)
      .then(setData)
      .catch(() => setData([]))
      .finally(() => setLoading(false));
  }, [forwardId, period]);

  const fmt = (ts: string) => {
    const d = new Date(ts);
    if (period === "7d")  return `${d.getMonth() + 1}/${d.getDate()} ${d.getHours()}:00`;
    if (period === "24h") return `${d.getHours()}:${String(d.getMinutes()).padStart(2, "0")}`;
    return `${d.getHours()}:${String(d.getMinutes()).padStart(2, "0")}`;
  };

  const fmtBytes = (n: number) => {
    if (n >= 1073741824) return `${(n / 1073741824).toFixed(1)} GB`;
    if (n >= 1048576)    return `${(n / 1048576).toFixed(1)} MB`;
    if (n >= 1024)       return `${(n / 1024).toFixed(1)} KB`;
    return `${n} B`;
  };

  const chartData = data.map((b) => ({
    ts: fmt(b.ts),
    下载: b.bytes_in,
    上传: b.bytes_out,
  }));

  return (
    <div className="space-y-4">
      <div className="flex gap-1">
        {(["1h", "24h", "7d"] as Period[]).map((p) => (
          <Button key={p} size="sm" variant={period === p ? "default" : "outline"}
            onClick={() => setPeriod(p)}>
            {p === "1h" ? "1 小时" : p === "24h" ? "24 小时" : "7 天"}
          </Button>
        ))}
      </div>
      {loading ? (
        <div className="h-48 flex items-center justify-center text-sm text-muted-foreground">加载中…</div>
      ) : data.length === 0 ? (
        <div className="h-48 flex items-center justify-center text-sm text-muted-foreground">
          暂无历史数据（数据每分钟写入一次）
        </div>
      ) : (
        <ResponsiveContainer width="100%" height={200}>
          <AreaChart data={chartData} margin={{ top: 4, right: 4, left: 4, bottom: 4 }}>
            <defs>
              <linearGradient id="gi" x1="0" y1="0" x2="0" y2="1">
                <stop offset="5%"  stopColor="hsl(142 76% 36%)" stopOpacity={0.3} />
                <stop offset="95%" stopColor="hsl(142 76% 36%)" stopOpacity={0} />
              </linearGradient>
              <linearGradient id="go" x1="0" y1="0" x2="0" y2="1">
                <stop offset="5%"  stopColor="hsl(199 78% 44%)" stopOpacity={0.3} />
                <stop offset="95%" stopColor="hsl(199 78% 44%)" stopOpacity={0} />
              </linearGradient>
            </defs>
            <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" />
            <XAxis dataKey="ts" tick={{ fontSize: 11 }} tickLine={false} />
            <YAxis tickFormatter={fmtBytes} tick={{ fontSize: 11 }} width={64} tickLine={false} axisLine={false} />
            <ReTooltip
              formatter={(v, name) => [fmtBytes(Number(v)), String(name)]}
              contentStyle={{ background: "hsl(var(--background))", border: "1px solid hsl(var(--border))", borderRadius: 8, fontSize: 12 }}
            />
            <Area type="monotone" dataKey="下载" stroke="hsl(142 76% 36%)" fill="url(#gi)" strokeWidth={1.5} dot={false} />
            <Area type="monotone" dataKey="上传" stroke="hsl(199 78% 44%)"  fill="url(#go)" strokeWidth={1.5} dot={false} />
          </AreaChart>
        </ResponsiveContainer>
      )}
    </div>
  );
}

// ── 上游延迟统计面板 ──────────────────────────────────────────────────────────

function UpstreamProbeStatsPanel({ forwardId }: { forwardId: string }) {
  const { data, isLoading } = useSWR(
    `forward-probe-stats-${forwardId}`,
    () => Api.getForwardProbeStats(forwardId),
    { refreshInterval: 60_000 },
  );

  const fmtLatency = (us: number | null) =>
    us == null ? "—" : us < 1000 ? `${us} µs` : `${(us / 1000).toFixed(1)} ms`;

  const latColor = (us: number | null) => {
    if (us == null) return "";
    const ms = us / 1000;
    return ms < 80 ? "text-emerald-500" : ms < 200 ? "text-amber-500" : "text-rose-500";
  };

  if (isLoading) {
    return <div className="py-8 text-center text-sm text-muted-foreground">加载中…</div>;
  }
  if (!data || data.length === 0) {
    return (
      <div className="py-8 text-center text-sm text-muted-foreground">
        暂无探测数据，等待首次探测（约 5 分钟）
      </div>
    );
  }

  return (
    <div className="space-y-3">
      {data.map((s) => (
        <div key={s.upstream_addr} className="rounded-lg border p-3 space-y-2">
          <div className="font-mono text-xs text-muted-foreground">{s.upstream_addr}</div>
          <div className="grid grid-cols-3 gap-3 text-sm">
            <div>
              <div className="text-xs text-muted-foreground mb-0.5">平均延迟</div>
              <div className={`font-semibold tabular-nums ${latColor(s.avg_latency_us)}`}>
                {fmtLatency(s.avg_latency_us)}
              </div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground mb-0.5">抖动</div>
              <div className="font-semibold tabular-nums">
                {fmtLatency(s.jitter_us)}
              </div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground mb-0.5">丢包率</div>
              <div className={`font-semibold tabular-nums ${s.loss_rate > 0.1 ? "text-rose-500" : s.loss_rate > 0 ? "text-amber-500" : "text-emerald-500"}`}>
                {(s.loss_rate * 100).toFixed(1)}%
              </div>
            </div>
          </div>
          <div className="text-xs text-muted-foreground">
            共 {s.sample_count} 次探测
            {s.last_probed_at && (
              <> · 最近 {new Date(s.last_probed_at).toLocaleString("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" })}</>
            )}
          </div>
        </div>
      ))}
    </div>
  );
}
