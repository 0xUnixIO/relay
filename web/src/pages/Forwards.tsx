import { useEffect, useMemo, useState } from "react";
import { useForwardStats } from "@/hooks/useForwardStats";
import { useConfirm } from "@/hooks/useConfirm";
import useSWR from "swr";
import {
  Plus, Pencil, Trash2, Pause, Play, RefreshCw, Activity, Shuffle, Check, Network, MoreHorizontal, BarChart2,
} from "lucide-react";
import { Card, CardContent } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
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
};

const emptyForm = (): Form => ({
  tunnel_id: "",
  name: "",
  in_port: "",
  remote_addrs: "",
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
        });
      } else {
        if (!form.tunnel_id) { toast.error("请选择隧道"); setSaving(false); return; }
        const created = await Api.createForward({
          tunnel_id: form.tunnel_id,
          name: form.name,
          in_port: inPort,
          remote_addrs: parsed,
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

  const colSpan = 8;

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

      <Card>
        <CardContent className="p-0">
          <ScrollArea>
            <Table className="min-w-[720px] table-fixed">
              <colgroup>
                <col style={{ width: "6rem" }} />
                <col style={{ width: "5rem" }} />
                <col style={{ width: "9rem" }} />
                <col />
                <col style={{ width: "5rem" }} />
                <col style={{ width: "6rem" }} />
                <col style={{ width: "6rem" }} />
                <col style={{ width: "8rem" }} />
              </colgroup>
              <TableHeader>
                <TableRow>
                  <TableHead>名称</TableHead>
                  <TableHead>隧道</TableHead>
                  <TableHead>入口</TableHead>
                  <TableHead>上游</TableHead>
                  <TableHead className="text-right">下载</TableHead>
                  <TableHead className="text-right">上传</TableHead>
                  <TableHead className="text-right whitespace-nowrap">活跃连接</TableHead>
                  <TableHead className="text-right">操作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {forwards.length === 0 ? (
                  <TableRow className="hover:bg-transparent even:bg-transparent">
                    <TableCell colSpan={colSpan} className="text-center">
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
                        <TableCell className="font-medium truncate">
                          <Tooltip>
                            <TooltipTrigger asChild>
                              <span className="truncate block w-full">{f.name}</span>
                            </TooltipTrigger>
                            <TooltipContent>{f.name}</TooltipContent>
                          </Tooltip>
                        </TableCell>
                        <TableCell className="text-xs truncate">
                          <Tooltip>
                            <TooltipTrigger asChild>
                              <span className="truncate block w-full">{f.tunnel_name}</span>
                            </TooltipTrigger>
                            <TooltipContent>{f.tunnel_name}</TooltipContent>
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
                                {copied === `${f.id}-port` && <Check className="h-3 w-3 text-emerald-500 shrink-0" />}
                              </span>
                            </TooltipTrigger>
                            <TooltipContent>
                              {f.entry_addrs && f.entry_addrs.length > 1
                                ? f.entry_addrs.join(" / ")
                                : (f.entry_addr ?? String(f.in_port))}
                            </TooltipContent>
                          </Tooltip>
                        </TableCell>
                        <TableCell className="font-mono text-xs text-muted-foreground">
                          <Tooltip>
                            <TooltipTrigger asChild>
                              <span
                                className="inline-flex items-center gap-1 min-w-0 max-w-full cursor-pointer hover:text-foreground"
                                onClick={() => copy(`${f.id}-upstream`, f.remote_addrs.join("\n"))}
                              >
                                <span className="truncate min-w-0">{f.remote_addrs.slice(0, 1).join(", ")}</span>
                                {f.remote_addrs.length > 1 && (
                                  <Badge variant="secondary" className="shrink-0">+{f.remote_addrs.length - 1}</Badge>
                                )}
                                {copied === `${f.id}-upstream` && <Check className="h-3 w-3 text-emerald-500 shrink-0" />}
                              </span>
                            </TooltipTrigger>
                            <TooltipContent>
                              {f.remote_addrs.join(" / ")}
                            </TooltipContent>
                          </Tooltip>
                        </TableCell>
                        <TableCell className="text-right font-mono text-xs whitespace-nowrap">
                          <div>{fmtBytes(f.in_flow_bytes)}</div>
                          {rates.get(f.id)?.inRate != null && rates.get(f.id)!.inRate > 0 && (
                            <div className="text-emerald-500">{fmtBytes(rates.get(f.id)!.inRate)}/s</div>
                          )}
                        </TableCell>
                        <TableCell className="text-right font-mono text-xs whitespace-nowrap">
                          <div>{fmtBytes(f.out_flow_bytes)}</div>
                          {rates.get(f.id)?.outRate != null && rates.get(f.id)!.outRate > 0 && (
                            <div className="text-sky-500">{fmtBytes(rates.get(f.id)!.outRate)}/s</div>
                          )}
                        </TableCell>
                        <TableCell className="text-right font-mono text-xs whitespace-nowrap">{f.active_connections}</TableCell>
                        <TableCell className="whitespace-nowrap text-right">
                          <div className="inline-flex items-center gap-1">
                            <Tooltip>
                              <TooltipTrigger asChild>
                                <Button size="icon" variant="ghost" onClick={() => setStatsTarget(f)}>
                                  <BarChart2 className="h-4 w-4" />
                                </Button>
                              </TooltipTrigger>
                              <TooltipContent>流量趋势</TooltipContent>
                            </Tooltip>
                            <Tooltip>
                              <TooltipTrigger asChild>
                                <Button size="icon" variant="ghost" onClick={() => probe(f)} disabled={!!probing[f.id]}>
                                  <Activity className={`h-4 w-4 ${probing[f.id] ? "animate-pulse" : ""}`} />
                                </Button>
                              </TooltipTrigger>
                              <TooltipContent>探测上游延迟</TooltipContent>
                            </Tooltip>
                            <Tooltip>
                              <TooltipTrigger asChild>
                                <Button size="icon" variant="ghost" onClick={() => openEdit(f)}>
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
          </ScrollArea>
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

// ── 延迟探测拓扑图 ─────────────────────────────────────────────────────────────

import type { ForwardProbeHop } from "@/lib/api";

function ProbeTopology({ hops }: { hops: ForwardProbeHop[] }) {
  const NODE_W = 72, NODE_H = 28, H_GAP = 100, V_GAP = 44, PAD = 16;

  // 构建节点 label map
  const labels = new Map<string, string>();
  for (const h of hops) {
    labels.set(h.from_node, h.from_node_name || h.from_node.slice(0, 8));
    const toId = h.to_node || h.target;
    if (toId) {
      const toLabel = h.to_node_name || (h.target ? h.target.split(":")[0].slice(0, 14) : toId.slice(0, 14));
      labels.set(toId, toLabel);
    }
  }

  // 构建边
  const edges = hops.flatMap((h) => {
    const toId = h.to_node || h.target;
    return toId ? [{ from: h.from_node, to: toId, us: h.latency_us, ok: h.ok }] : [];
  });

  // 拓扑排序分层（BFS）
  const inDegree = new Map<string, number>();
  const adj = new Map<string, string[]>();
  for (const e of edges) {
    inDegree.set(e.to, (inDegree.get(e.to) ?? 0) + 1);
    const list = adj.get(e.from) ?? [];
    list.push(e.to);
    adj.set(e.from, list);
  }
  const layerOf = new Map<string, number>();
  const q = [...labels.keys()].filter((n) => !(inDegree.get(n) ?? 0));
  q.forEach((n) => layerOf.set(n, 0));
  for (let i = 0; i < q.length; i++) {
    const n = q[i];
    for (const next of adj.get(n) ?? []) {
      const l = Math.max(layerOf.get(next) ?? 0, (layerOf.get(n) ?? 0) + 1);
      layerOf.set(next, l);
      if (!q.includes(next)) q.push(next);
    }
  }

  // 按层分组
  const layers = new Map<number, string[]>();
  for (const [id, l] of layerOf) {
    const arr = layers.get(l) ?? [];
    arr.push(id);
    layers.set(l, arr);
  }
  const numLayers = Math.max(...layerOf.values()) + 1;
  const maxPerLayer = Math.max(...[...layers.values()].map((a) => a.length));

  const svgW = PAD * 2 + numLayers * NODE_W + (numLayers - 1) * H_GAP;
  const svgH = PAD * 2 + maxPerLayer * NODE_H + (maxPerLayer - 1) * V_GAP;

  // 节点坐标
  const pos = new Map<string, { x: number; y: number }>();
  for (const [l, nodes] of layers) {
    const x = PAD + l * (NODE_W + H_GAP);
    const totalH = nodes.length * NODE_H + (nodes.length - 1) * V_GAP;
    const startY = PAD + (svgH - PAD * 2 - totalH) / 2;
    nodes.forEach((id, i) => pos.set(id, { x, y: startY + i * (NODE_H + V_GAP) }));
  }

  const latColor = (us: number) => {
    const ms = us / 1000;
    return ms < 80 ? "#059669" : ms < 200 ? "#d97706" : "#e11d48";
  };

  // 合计（并行取最大，串行求和）
  const allOk = hops.every((h) => h.ok);
  const byTo = new Map<string, number>();
  for (const h of hops) {
    if (!h.ok) continue;
    const key = h.to_node || h.target || h.from_node;
    byTo.set(key, Math.max(byTo.get(key) ?? 0, h.latency_us));
  }
  const total = Array.from(byTo.values()).reduce((s, v) => s + v, 0);
  const totalColor = total / 1000 < 80 ? "#059669" : total / 1000 < 200 ? "#d97706" : "#e11d48";

  return (
    <div className="space-y-2">
      <svg
        viewBox={`0 0 ${svgW} ${svgH}`}
        width={svgW}
        height={svgH}
        className="mx-auto overflow-visible"
        style={{ maxWidth: "100%" }}
      >
        {edges.map((e, i) => {
          const f = pos.get(e.from), t = pos.get(e.to);
          if (!f || !t) return null;
          const x1 = f.x + NODE_W, y1 = f.y + NODE_H / 2;
          const x2 = t.x, y2 = t.y + NODE_H / 2;
          const mx = (x1 + x2) / 2;
          const color = e.ok ? latColor(e.us) : "#e11d48";
          const midY = (y1 + y2) / 2;
          return (
            <g key={i}>
              <path
                d={`M${x1} ${y1} C${mx} ${y1} ${mx} ${y2} ${x2} ${y2}`}
                fill="none" stroke={color} strokeWidth={1.5} opacity={0.8}
                strokeDasharray={e.ok ? undefined : "4 2"}
              />
              <text
                x={mx} y={midY - 4}
                textAnchor="middle" fontSize={9} fill={color}
                fontFamily="ui-monospace,monospace" fontWeight={600}
              >
                {e.ok ? `${(e.us / 1000).toFixed(1)}ms` : "✗"}
              </text>
            </g>
          );
        })}
        {[...pos.entries()].map(([id, { x, y }]) => (
          <g key={id}>
            <rect x={x} y={y} width={NODE_W} height={NODE_H} rx={6}
              fill="hsl(var(--muted))" stroke="hsl(var(--border))" strokeWidth={1} />
            <text
              x={x + NODE_W / 2} y={y + NODE_H / 2 + 4}
              textAnchor="middle" fontSize={11} fontWeight={500}
              fill="hsl(var(--foreground))"
            >
              {(labels.get(id) ?? id).slice(0, 10)}
            </text>
          </g>
        ))}
      </svg>
      {allOk && hops.length > 0 && (
        <div className="flex items-center justify-between border-t pt-2 text-sm">
          <span className="text-muted-foreground">合计</span>
          <span className="font-mono tabular-nums font-semibold" style={{ color: totalColor }}>
            {(total / 1000).toFixed(1)} ms
          </span>
        </div>
      )}
    </div>
  );
}

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
