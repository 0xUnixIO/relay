import { useEffect, useRef, useState } from "react";
import { useConfirm } from "@/hooks/useConfirm";
import useSWR from "swr";
import {
  Plus, Pencil, Trash2, Activity, Route as RouteIcon, ChevronDown,
} from "lucide-react";
import { Card, CardContent } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
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
import { cn } from "@/lib/utils";
import { Api, type Tunnel, type NodeInfo, type TunnelProbeSegment } from "@/lib/api";
import { getToken } from "@/lib/auth";
import { toast } from "sonner";

type Form = {
  name: string;
  description: string;
  protocols: ("tcp" | "udp")[];
  ip_preference: string;
  in_ip: string;
  layers: string[][];
  enabled: boolean;
};

const emptyForm = (): Form => ({
  name: "",
  description: "",
  protocols: ["tcp", "udp"],
  ip_preference: "",
  in_ip: "",
  layers: [],
  enabled: true,
});

/** 判断节点是否在线：last_seen_at 在 15 秒内 */
const isOnline = (n: NodeInfo) =>
  !!n.last_seen_at && Date.now() - new Date(n.last_seen_at).getTime() < 15_000;


export default function TunnelsPage() {
  const confirm = useConfirm();
  const { data: tunnels = [], mutate } = useSWR("tunnels", Api.listTunnels);
  const { data: nodes = [] } = useSWR("nodes", Api.listNodes);

  const [open, setOpen] = useState(false);
  const [editing, setEditing] = useState<Tunnel | null>(null);
  const [form, setForm] = useState<Form>(emptyForm());
  const [saving, setSaving] = useState(false);
  const [probeOpen, setProbeOpen] = useState(false);
  const [probeTunnel, setProbeTunnel] = useState<Tunnel | null>(null);
  const [probeStreaming, setProbeStreaming] = useState(false);
  const [probeSegments, setProbeSegments] = useState<TunnelProbeSegment[]>([]);

  const openNew = () => {
    setEditing(null);
    setForm(emptyForm());
    setOpen(true);
  };

  const openEdit = (t: Tunnel) => {
    setEditing(t);
    let layers: string[][];
    if (t.layers && t.layers.length > 0) {
      layers = t.layers;
    } else {
      const sorted = (t.hops ?? []).slice().sort((a, b) => a.hop_index - b.hop_index);
      const byHop: Record<number, string[]> = {};
      for (const h of sorted) {
        if (!byHop[h.hop_index]) byHop[h.hop_index] = [];
        byHop[h.hop_index].push(h.node_id);
      }
      const maxIdx = sorted.length > 0 ? Math.max(...sorted.map((h) => h.hop_index)) : -1;
      layers = Array.from({ length: maxIdx + 1 }, (_, i) => byHop[i] ?? []);
    }
    setForm({
      name: t.name,
      description: t.description ?? "",
      protocols: t.protocols ?? ["tcp", "udp"],
      ip_preference: t.ip_preference ?? "",
      in_ip: t.in_ip ?? "",
      layers,
      enabled: t.enabled,
    });
    setOpen(true);
  };

  const submit = async () => {
    const protocolsLocked = !!editing && editing.forward_count > 0;
    const activeLayers = form.layers.filter((l) => l.length > 0);
    if (!form.name.trim() || activeLayers.length === 0) {
      toast.error("请填写名称并至少添加一个节点");
      return;
    }
    if (form.protocols.length === 0) {
      toast.error("至少选择一个协议");
      return;
    }
    setSaving(true);
    try {
      if (editing) {
        await Api.updateTunnel(editing.id, {
          name: form.name,
          description: form.description,
          ip_preference: form.ip_preference || undefined,
          in_ip: form.in_ip || undefined,
          enabled: form.enabled,
          ...(protocolsLocked ? {} : { protocols: form.protocols }),
          layers: activeLayers,
        });
      } else {
        await Api.createTunnel({
          name: form.name,
          description: form.description,
          protocols: form.protocols,
          ip_preference: form.ip_preference || undefined,
          in_ip: form.in_ip || undefined,
          enabled: form.enabled,
          layers: activeLayers,
        });
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

  const remove = async (t: Tunnel) => {
    if (!await confirm(`确认删除隧道 "${t.name}"？`)) return;
    try {
      await Api.deleteTunnel(t.id);
      mutate();
    } catch (e: any) {
      toast.error(e?.message ?? "删除失败");
    }
  };

  const openProbe = async (t: Tunnel) => {
    setProbeTunnel(t);
    setProbeSegments([]);
    setProbeStreaming(true);
    setProbeOpen(true);
    try {
      const token = getToken();
      const res = await fetch(`/api/v1/tunnels/${t.id}/probe-stream`, {
        headers: token ? { authorization: `Bearer ${token}` } : {},
      });
      if (!res.ok || !res.body) { toast.error("探测失败"); setProbeOpen(false); return; }
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buf = "";
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        const lines = buf.split("\n");
        buf = lines.pop()!;
        for (const line of lines) {
          if (!line.startsWith("data: ")) continue;
          try {
            const seg = JSON.parse(line.slice(6)) as TunnelProbeSegment;
            setProbeSegments((prev) => [...prev, seg]);
          } catch {}
        }
      }
    } catch (e: any) {
      toast.error(e?.message ?? "探测失败");
      setProbeOpen(false);
    } finally {
      setProbeStreaming(false);
    }
  };

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold">隧道</h1>
        <Button onClick={openNew}>
          <Plus className="mr-1 h-4 w-4" /> 新建隧道
        </Button>
      </div>

      <Card>
        <CardContent className="p-0">
          <ScrollArea>
            <Table className="min-w-[640px] table-fixed">
              <colgroup>
                <col style={{ width: "10rem" }} />
                <col />
                <col style={{ width: "5rem" }} />
                <col style={{ width: "5rem" }} />
                <col style={{ width: "8rem" }} />
              </colgroup>
              <TableHeader>
                <TableRow>
                  <TableHead>名称</TableHead>
                  <TableHead>路径</TableHead>
                  <TableHead>转发数</TableHead>
                  <TableHead>启用</TableHead>
                  <TableHead>操作</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {tunnels.length === 0 ? (
                  <TableRow className="hover:bg-transparent even:bg-transparent">
                    <TableCell colSpan={5} className="text-center">
                      <EmptyState icon={RouteIcon} title="暂无隧道" description="点击右上角「新建隧道」按钮创建。" compact />
                    </TableCell>
                  </TableRow>
                ) : (
                  tunnels.map((t) => {
                    const hopLabels = (() => {
                      if (t.layers && t.layers.length > 0) {
                        return t.layers.map((layer) => {
                          const names = layer.map((id) => nodes.find((nn) => nn.id === id)?.hostname || id);
                          return names.length > 1 ? `[${names.join("|")}]` : (names[0] ?? layer[0]);
                        });
                      }
                      return [...(t.hops ?? [])]
                        .sort((a, b) => a.hop_index - b.hop_index)
                        .map((h, i, arr) => {
                          // 同 hop_index 只显示一次
                          if (i > 0 && arr[i - 1].hop_index === h.hop_index) return null;
                          const n = nodes.find((nn) => nn.id === h.node_id);
                          return n?.hostname || h.node_id;
                        })
                        .filter(Boolean) as string[];
                    })();
                    const hopTitle = hopLabels.join(" → ");
                    return (
                    <TableRow key={t.id}>
                      <TableCell className="font-medium truncate" title={t.name}>{t.name}</TableCell>
                      <TableCell className="text-xs truncate" title={hopTitle}>
                        {hopTitle}
                      </TableCell>
                      <TableCell>{t.forward_count}</TableCell>
                      <TableCell>
                        {t.enabled ? (
                          <Badge variant="success">启用</Badge>
                        ) : (
                          <Badge variant="outline">停用</Badge>
                        )}
                      </TableCell>
                      <TableCell className="space-x-1 whitespace-nowrap">
                        <Button
                          size="icon"
                          variant="ghost"
                          onClick={() => openProbe(t)}
                          disabled={probeStreaming && probeTunnel?.id === t.id}
                          aria-label="连通性检测"
                        >
                          <Activity className={`h-4 w-4 ${probeStreaming && probeTunnel?.id === t.id ? "animate-pulse" : ""}`} />
                        </Button>
                        <Button
                          size="icon"
                          variant="ghost"
                          onClick={() => openEdit(t)}
                          aria-label="编辑"
                        >
                          <Pencil className="h-4 w-4" />
                        </Button>
                        <Button
                          size="icon"
                          variant="ghost"
                          onClick={() => remove(t)}
                          aria-label="删除"
                        >
                          <Trash2 className="h-4 w-4" />
                        </Button>
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

      {/* 连通性检测对话框 */}
      <Dialog open={probeOpen} onOpenChange={(o) => { setProbeOpen(o); if (!o) setProbeSegments([]); }}>
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>连通性检测 — {probeTunnel?.name}</DialogTitle>
            <DialogDescription>逐跳探测各段网络可达性</DialogDescription>
          </DialogHeader>
          <TunnelProbeTopology
            segments={probeSegments}
            streaming={probeStreaming}
            nodeName={(id) => nodes.find((n) => n.id === id)?.hostname || id}
          />
          <DialogFooter>
            <Button variant="ghost" onClick={() => setProbeOpen(false)}>关闭</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 新建/编辑隧道对话框 */}
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>{editing ? "编辑隧道" : "新建隧道"}</DialogTitle>
            <DialogDescription>
              按顺序添加节点：第一个为入口，最后一个为出口。每跳可配置多台节点实现并行冗余。
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-5">
            {/* 第一行：名称 + 描述 */}
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
              <div className="space-y-1.5">
                <Label>名称</Label>
                <Input
                  value={form.name}
                  onChange={(e) => setForm({ ...form, name: e.target.value })}
                />
              </div>
              <div className="space-y-1.5">
                <Label>说明</Label>
                <Input
                  value={form.description}
                  onChange={(e) => setForm({ ...form, description: e.target.value })}
                  placeholder="可选"
                />
              </div>
            </div>

            {/* 第二行：协议多选（默认 TCP+UDP；编辑且已被使用时锁定） */}
            <div className="space-y-1.5">
              <div className="flex items-center gap-3 flex-wrap">
                <Label className="shrink-0">协议</Label>
                <div className="flex gap-1">
                  {(["tcp", "udp"] as const).map((p) => {
                    const checked = form.protocols.includes(p);
                    const protocolsLocked = !!editing && editing.forward_count > 0;
                    return (
                      <button
                        key={p}
                        type="button"
                        disabled={protocolsLocked}
                        onClick={() => {
                          const next = checked
                            ? form.protocols.filter((x) => x !== p)
                            : [...form.protocols, p];
                          if (next.length === 0) {
                            toast.error("至少选择一个协议");
                            return;
                          }
                          setForm({ ...form, protocols: next as ("tcp" | "udp")[] });
                        }}
                        className={cn(
                          "rounded border px-2 py-0.5 text-xs font-mono font-medium transition-colors",
                          checked
                            ? "border-primary bg-primary text-primary-foreground"
                            : "border-input bg-background text-muted-foreground hover:border-primary/50",
                          protocolsLocked && "opacity-50 cursor-not-allowed",
                        )}
                      >
                        {p.toUpperCase()}
                      </button>
                    );
                  })}
                </div>
                <span className="text-sm text-muted-foreground">
                  至少选一项；默认双协议（TCP+UDP）共享同一监听端口
                </span>
              </div>
              {!!editing && editing.forward_count > 0 && (
                <p className="text-sm text-muted-foreground">
                  已有 {editing.forward_count} 个转发使用此隧道，无法修改协议。
                </p>
              )}
            </div>

            {/* 第三行：链路配置（HopEditor） */}
            {!!editing && editing.forward_count > 0 && (
              <p className="text-sm text-amber-600 dark:text-amber-400">
                已有 {editing.forward_count} 个转发使用此隧道，修改路径后转发将在下次重连时生效。
              </p>
            )}
            <HopEditor
              nodes={nodes}
              layers={form.layers}
              onChange={(ls) => setForm({ ...form, layers: ls })}
            />

            {/* 高级设置（可折叠） */}
            <AdvancedSettings form={form} setForm={setForm} />
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
    </div>
  );
}

/* ------------------------------------------------------------------ */
/*  高级设置折叠面板                                                     */
/* ------------------------------------------------------------------ */

function AdvancedSettings({
  form,
  setForm,
}: {
  form: Form;
  setForm: (f: Form) => void;
}) {
  const [open, setOpen] = useState(false);

  return (
    <div className="rounded-md border">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center justify-between px-3 py-2 text-sm font-medium text-muted-foreground hover:text-foreground transition-colors"
        aria-expanded={open}
      >
        <span>高级设置</span>
        <ChevronDown
          className={cn("h-4 w-4 transition-transform duration-200", open && "rotate-180")}
        />
      </button>

      {open && (
        <div className="border-t px-3 py-3 space-y-3">
          <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <Label>IP 偏好</Label>
              <Select
                value={form.ip_preference || "auto"}
                onValueChange={(v) =>
                  setForm({ ...form, ip_preference: v === "auto" ? "" : v })
                }
              >
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="auto">auto</SelectItem>
                  <SelectItem value="ipv4">ipv4</SelectItem>
                  <SelectItem value="ipv6">ipv6</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div className="space-y-1.5">
              <Label>入口地址（可选）</Label>
              <Input
                value={form.in_ip}
                onChange={(e) => setForm({ ...form, in_ip: e.target.value })}
                placeholder="域名或 IP，留空使用节点首个 IP"
              />
            </div>
          </div>

          <label className="flex items-center gap-2 text-sm cursor-pointer select-none">
            <input
              type="checkbox"
              checked={form.enabled}
              onChange={(e) => setForm({ ...form, enabled: e.target.checked })}
              className="rounded"
            />
            启用此隧道（停用将暂停所有派生转发）
          </label>
        </div>
      )}
    </div>
  );
}

/* ------------------------------------------------------------------ */
/*  HopEditor — SVG 拓扑图 + 底部节点选择                                  */
/* ------------------------------------------------------------------ */

type PickerTarget = { type: "add"; li: number } | { type: "new" };

// 按 hostname 估算节点框宽度（含左侧状态点 + 右侧删除按钮的空间）
function hopNodeW(label: string) {
  return Math.max(96, Math.ceil(label.length * 7.2) + 48);
}

function HopEditor({
  nodes,
  layers,
  onChange,
  disabled = false,
  disabledReason,
}: {
  nodes: NodeInfo[];
  layers: string[][];
  onChange: (layers: string[][]) => void;
  disabled?: boolean;
  disabledReason?: string;
}) {
  const [picker, setPicker] = useState<PickerTarget | null>(null);
  const [search, setSearch] = useState("");
  const [hoverDel, setHoverDel] = useState<string | null>(null);

  // 切换 picker 时清空搜索
  useEffect(() => { setSearch(""); }, [picker]);

  const NH = 30, HG = 80, VG = 10, PX = 20, PY = 12;
  const HEADER_H = 20; // 列标题高度
  const ABR = 11, ABM = 8; // + 圆钮半径和上间距
  const GNW = 96; // 新增跳幽灵框宽度

  // 节点标签
  const nodeLabel = (id: string) => nodes.find((n) => n.id === id)?.hostname || id;
  // 每列宽度（取该列所有节点标签的最大估算宽度）
  const colW = (li: number) => {
    const layer = layers[li] ?? [];
    return layer.length > 0 ? Math.max(...layer.map((id) => hopNodeW(nodeLabel(id)))) : 96;
  };
  const colWidths = layers.map((_, li) => colW(li));
  // 累计 x 偏移
  const colX = (i: number): number => {
    let x = PX;
    for (let j = 0; j < i; j++) x += colWidths[j] + HG;
    return x;
  };

  const allIds = new Set(layers.flat());

  const addToLayer = (li: number, id: string) => {
    onChange(layers.map((l, i) => (i === li ? [...l, id] : l)));
    setPicker(null);
  };
  const removeFromLayer = (li: number, id: string) =>
    onChange(
      layers
        .map((l, i) => (i === li ? l.filter((x) => x !== id) : l))
        .filter((l) => l.length > 0),
    );
  const addNewLayer = (id: string) => {
    onChange([...layers, [id]]);
    setPicker(null);
  };
  const toggle = (next: PickerTarget) =>
    setPicker((prev) => {
      if (!prev) return next;
      if (next.type === "new" && prev.type === "new") return null;
      if (next.type === "add" && prev.type === "add" && next.li === prev.li) return null;
      return next;
    });

  const maxNodes = layers.length > 0 ? Math.max(...layers.map((l) => l.length)) : 1;
  const dataH = maxNodes * NH + (maxNodes - 1) * VG;
  const extraH = disabled ? 0 : ABM + ABR * 2;
  const svgH = PY + HEADER_H + dataH + PY + extraH;
  const svgW =
    layers.length === 0
      ? PX * 2 + GNW
      : colX(layers.length - 1) + colWidths[layers.length - 1] + (disabled ? PX : HG + GNW + PX);

  const nodeYs = (layer: string[]) => {
    const th = layer.length * NH + (layer.length - 1) * VG;
    const sy = PY + HEADER_H + (dataH - th) / 2;
    return layer.map((_, j) => sy + j * (NH + VG));
  };
  const groupCenterY = (layer: string[]) => {
    const ys = nodeYs(layer);
    if (ys.length === 0) return PY + HEADER_H + dataH / 2;
    return (ys[0] + ys[ys.length - 1] + NH) / 2;
  };

  const colLabel = (li: number) => {
    if (li === 0) return "入口";
    if (li === layers.length - 1 && layers.length > 1) return "出口";
    return `Hop ${li + 1}`;
  };
  const pickerTitle = () => {
    if (!picker) return "";
    if (picker.type === "new") return "新建跳";
    const li = picker.li;
    if (li === 0) return "添加到入口";
    if (li === layers.length - 1) return "添加到出口";
    return `添加到 Hop ${li + 1}`;
  };

  const baseOpts = nodes.filter((n) => n.tunnel_eligible && !allIds.has(n.id));
  const pickerOpts = search
    ? baseOpts.filter((n) => (n.hostname || n.id).toLowerCase().includes(search.toLowerCase()))
    : baseOpts;

  if (disabled && layers.length === 0) {
    return (
      <div className="space-y-3">
        <Label>链路配置</Label>
        {disabledReason && (
          <div className="text-sm rounded border border-amber-500/40 bg-amber-500/10 text-amber-700 dark:text-amber-300 px-2 py-1.5">
            {disabledReason}
          </div>
        )}
        <div className="rounded-md border bg-muted/20 px-3 py-2 text-sm text-muted-foreground">链路为空</div>
      </div>
    );
  }

  return (
    <div className="space-y-3">
      <Label>链路配置</Label>
      {disabled && disabledReason && (
        <div className="text-sm rounded border border-amber-500/40 bg-amber-500/10 text-amber-700 dark:text-amber-300 px-2 py-1.5">
          {disabledReason}
        </div>
      )}

      <div className="rounded-md border bg-muted/20">
        <ScrollArea className="p-3">
          <svg width={svgW} height={svgH} className="overflow-visible">

            {/* ── 活跃列高亮 ── */}
            {!disabled && picker?.type === "add" &&
              layers.map((_, li) => {
                if (picker.li !== li) return null;
                const cw = colWidths[li];
                return (
                  <rect key={`hl-${li}`}
                    x={colX(li) - 6} y={PY + HEADER_H - 6}
                    width={cw + 12} height={dataH + 12}
                    rx={8}
                    fill="hsl(var(--primary) / 0.06)"
                    stroke="hsl(var(--primary) / 0.25)"
                    strokeWidth={1}
                  />
                );
              })}

            {/* ── 列标题 ── */}
            {layers.map((_, li) => (
              <text key={`hdr-${li}`}
                x={colX(li) + colWidths[li] / 2}
                y={PY + HEADER_H - 6}
                textAnchor="middle" fontSize={10} fontWeight={500}
                fill={
                  picker?.type === "add" && picker.li === li
                    ? "hsl(var(--primary))"
                    : "hsl(var(--muted-foreground))"
                }
              >
                {colLabel(li)}
              </text>
            ))}

            {/* ── 跨层连线 ── */}
            {layers.slice(0, -1).map((layer, li) => {
              const x1 = colX(li) + colWidths[li], y1 = groupCenterY(layer);
              const x2 = colX(li + 1), y2 = groupCenterY(layers[li + 1]);
              const mx = (x1 + x2) / 2;
              return (
                <path key={`edge-${li}`}
                  d={`M${x1} ${y1} C${mx} ${y1} ${mx} ${y2} ${x2} ${y2}`}
                  fill="none" stroke="hsl(var(--border))" strokeWidth={1.5} />
              );
            })}

            {/* ── 虚线：最后一列 → 新增跳幽灵框 ── */}
            {!disabled && layers.length > 0 && (() => {
              const last = layers[layers.length - 1];
              const x1 = colX(layers.length - 1) + colWidths[layers.length - 1];
              const y1 = groupCenterY(last);
              const x2 = colX(layers.length);
              const y2 = PY + HEADER_H + (dataH - NH) / 2 + NH / 2;
              const mx = (x1 + x2) / 2;
              return (
                <path d={`M${x1} ${y1} C${mx} ${y1} ${mx} ${y2} ${x2} ${y2}`}
                  fill="none" stroke="hsl(var(--border))" strokeWidth={1.5} strokeDasharray="4 2" />
              );
            })()}

            {/* ── 节点框 ── */}
            {layers.map((layer, li) =>
              nodeYs(layer).map((y, ni) => {
                const nodeId = layer[ni];
                const node = nodes.find((n) => n.id === nodeId);
                const online = node ? isOnline(node) : false;
                const x = colX(li);
                const cw = colWidths[li];
                const label = nodeLabel(nodeId);
                const isDel = hoverDel === nodeId;
                return (
                  <g key={nodeId}>
                    <title>{label}</title>
                    <rect x={x} y={y} width={cw} height={NH} rx={6}
                      fill="hsl(var(--background))" stroke="hsl(var(--border))" strokeWidth={1} />
                    {/* 在线状态点 */}
                    <circle cx={x + 10} cy={y + NH / 2} r={3}
                      fill={online ? "#10b981" : "hsl(var(--muted-foreground))"}
                      fillOpacity={online ? 1 : 0.35} />
                    {/* 节点名（全显示，宽度自适应） */}
                    <text x={x + 19} y={y + NH / 2 + 4} fontSize={11}
                      fontFamily="ui-monospace, monospace" fill="hsl(var(--foreground))">
                      {label}
                    </text>
                    {/* 删除按钮 */}
                    {!disabled && (
                      <g
                        onClick={() => removeFromLayer(li, nodeId)}
                        onMouseEnter={() => setHoverDel(nodeId)}
                        onMouseLeave={() => setHoverDel(null)}
                        style={{ cursor: "pointer" }}
                      >
                        <rect x={x + cw - 20} y={y + 4} width={18} height={NH - 8} rx={3}
                          fill={isDel ? "hsl(var(--destructive) / 0.1)" : "transparent"} />
                        <text x={x + cw - 11} y={y + NH / 2 + 4.5} textAnchor="middle"
                          fontSize={13} fontFamily="sans-serif"
                          fill={isDel ? "hsl(var(--destructive))" : "hsl(var(--muted-foreground))"}>
                          ×
                        </text>
                      </g>
                    )}
                  </g>
                );
              })
            )}

            {/* ── 并联数标记（列右上角） ── */}
            {layers.map((layer, li) =>
              layer.length > 1 ? (
                <text key={`cnt-${li}`}
                  x={colX(li) + colWidths[li] - 4} y={PY + HEADER_H - 6}
                  textAnchor="end" fontSize={10} fontWeight={600}
                  fill="hsl(var(--muted-foreground))">
                  ×{layer.length}
                </text>
              ) : null
            )}

            {/* ── 每列下方 + 圆钮（添加节点到该列） ── */}
            {!disabled && layers.map((layer, li) => {
              const ys = nodeYs(layer);
              const cy = ys[ys.length - 1] + NH + ABM + ABR;
              const cx = colX(li) + colWidths[li] / 2;
              const active = picker?.type === "add" && picker.li === li;
              return (
                <g key={`plus-${li}`} onClick={() => toggle({ type: "add", li })} style={{ cursor: "pointer" }}>
                  <circle cx={cx} cy={cy} r={ABR}
                    fill={active ? "hsl(var(--primary) / 0.1)" : "hsl(var(--background))"}
                    stroke={active ? "hsl(var(--primary))" : "hsl(var(--border))"}
                    strokeWidth={active ? 1.5 : 1}
                    strokeDasharray={active ? undefined : "3 2"} />
                  <text x={cx} y={cy + 4.5} textAnchor="middle" fontSize={13}
                    fontFamily="sans-serif"
                    fill={active ? "hsl(var(--primary))" : "hsl(var(--muted-foreground))"}>+</text>
                </g>
              );
            })}

            {/* ── 新增跳幽灵框 ── */}
            {!disabled && (() => {
              const active = picker?.type === "new";
              const sx = layers.length === 0 ? PX : colX(layers.length);
              const sy = PY + HEADER_H + (dataH - NH) / 2;
              return (
                <g onClick={() => toggle({ type: "new" })} style={{ cursor: "pointer" }}>
                  <rect x={sx} y={sy} width={GNW} height={NH} rx={6}
                    fill={active ? "hsl(var(--primary) / 0.08)" : "hsl(var(--background))"}
                    stroke={active ? "hsl(var(--primary))" : "hsl(var(--border))"}
                    strokeWidth={active ? 1.5 : 1}
                    strokeDasharray={active ? undefined : "4 2"} />
                  <text x={sx + GNW / 2} y={sy + NH / 2} textAnchor="middle"
                    fontSize={13} fontFamily="sans-serif" dominantBaseline="central"
                    fill={active ? "hsl(var(--primary))" : "hsl(var(--muted-foreground))"}>+</text>
                  <text x={sx + GNW / 2} y={sy + NH + 10} textAnchor="middle"
                    fontSize={9}
                    fill={active ? "hsl(var(--primary) / 0.7)" : "hsl(var(--muted-foreground) / 0.6)"}>
                    新增跳
                  </text>
                </g>
              );
            })()}
          </svg>
        </ScrollArea>

        {/* ── Picker 面板 ── */}
        {picker && (
          <div className="border-t">
            {/* 标题 + 搜索框 */}
            <div className="flex items-center gap-2 px-3 pt-2 pb-1.5">
              <span className="text-xs font-medium text-muted-foreground shrink-0">
                {pickerTitle()}
              </span>
              <input
                autoFocus
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                placeholder="搜索节点…"
                className="ml-auto h-6 w-32 rounded border bg-background px-2 text-xs outline-none focus:ring-1 focus:ring-primary/50"
              />
            </div>
            {/* 节点列表 */}
            <div className="px-3 pb-2 flex flex-wrap gap-1.5">
              {pickerOpts.length > 0
                ? pickerOpts.map((n) => {
                    const online = isOnline(n);
                    return (
                      <button
                        key={n.id}
                        type="button"
                        onClick={() =>
                          picker.type === "add"
                            ? addToLayer(picker.li, n.id)
                            : addNewLayer(n.id)
                        }
                        className={cn(
                          "flex items-center gap-1.5 rounded border px-2 py-1 text-xs transition-colors hover:bg-accent",
                          !online && "opacity-60",
                        )}
                      >
                        <span className={cn("h-1.5 w-1.5 rounded-full shrink-0",
                          online ? "bg-emerald-500" : "bg-muted-foreground/40")} />
                        <span className="font-mono">{n.hostname || n.id}</span>
                      </button>
                    );
                  })
                : (
                  <span className="text-xs text-muted-foreground py-0.5">
                    {search ? "无匹配节点" : "无可用节点"}
                  </span>
                )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

// ── 隧道探测拓扑图（支持 pan/zoom，流式渲染）──────────────────────────────────

// 将 scale 夹在 [min, max] 范围内
function clamp(v: number, min: number, max: number) {
  return Math.min(max, Math.max(min, v));
}

// 根据文本长度估算节点宽度
function tunnelNodeWidth(label: string) {
  return Math.max(72, label.length * 7.5 + 20);
}

interface DragOrigin {
  ox: number;
  oy: number;
  otx: number;
  oty: number;
}

interface Transform {
  scale: number;
  tx: number;
  ty: number;
}

function TunnelProbeTopology({
  segments,
  streaming,
  nodeName,
}: {
  segments: TunnelProbeSegment[];
  streaming?: boolean;
  nodeName: (id: string) => string;
}) {
  const NODE_H = 28, H_GAP = 100, V_GAP = 44, PAD = 16;

  const containerRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<DragOrigin | null>(null);
  const [t, setT] = useState<Transform>({ scale: 1, tx: 0, ty: 0 });
  const fittedRef = useRef(false);
  const svgSizeRef = useRef({ w: 0, h: 0 });

  useEffect(() => {
    if (segments.length === 0) {
      fittedRef.current = false;
      return;
    }
    // 流结束时重置，确保最终布局（所有节点到齐后）再做一次 fit
    if (!streaming) fittedRef.current = false;
    if (fittedRef.current) return;

    const el = containerRef.current;
    if (!el) return;

    const tryFit = () => {
      const { w, h } = svgSizeRef.current;
      const rect = el.getBoundingClientRect();
      if (rect.width === 0 || w === 0 || h === 0) return false;
      fittedRef.current = true;
      const fit = Math.min(rect.width / w, rect.height / h);
      const s = clamp(fit >= 1 ? 1 : fit * 0.9, 0.15, 1);
      setT({ scale: s, tx: (rect.width - w * s) / 2, ty: (rect.height - h * s) / 2 });
      return true;
    };

    if (tryFit()) return;
    const ro = new ResizeObserver(() => { if (tryFit()) ro.disconnect(); });
    ro.observe(el);
    return () => ro.disconnect();
  }, [segments.length, streaming]);

  // 无数据且仍在流式探测时，展示等待提示
  if (segments.length === 0 && streaming) {
    return (
      <div className="flex h-24 items-center justify-center gap-2 text-sm text-muted-foreground">
        <span className="h-4 w-4 rounded-full border-2 border-primary border-t-transparent animate-spin" />
        探测中，等待第一段结果…
      </div>
    );
  }

  // 构建节点标签（完整显示，不截断）
  const labels = new Map<string, string>();
  for (const s of segments) {
    labels.set(s.from_node, nodeName(s.from_node));
    if (s.to) labels.set(s.to, nodeName(s.to));
  }

  const edges = segments.flatMap((s) =>
    s.to ? [{ from: s.from_node, to: s.to, us: s.latency_us, ok: s.ok, err: s.error }] : [],
  );

  // 拓扑排序分层（BFS / longest-path）
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

  const layerMap = new Map<number, string[]>();
  for (const [id, l] of layerOf) {
    const arr = layerMap.get(l) ?? [];
    arr.push(id);
    layerMap.set(l, arr);
  }

  // 空 segments 且不在流式探测 => 无内容
  if (layerOf.size === 0) {
    return null;
  }

  const numLayers = Math.max(...layerOf.values()) + 1;
  const maxPerLayer = Math.max(...[...layerMap.values()].map((a) => a.length));

  // 每层节点宽度（取该层最宽标签）
  const layerWidths = new Map<number, number>();
  for (const [l, nodeIds] of layerMap) {
    const w = Math.max(...nodeIds.map((id) => tunnelNodeWidth(labels.get(id) ?? id)));
    layerWidths.set(l, w);
  }

  // 各层 x 偏移
  const layerX: number[] = [];
  let curX = PAD;
  for (let l = 0; l < numLayers; l++) {
    layerX[l] = curX;
    curX += (layerWidths.get(l) ?? 72) + H_GAP;
  }

  const svgW = curX - H_GAP + PAD;
  const svgH = PAD * 2 + maxPerLayer * NODE_H + (maxPerLayer - 1) * V_GAP;
  svgSizeRef.current = { w: svgW, h: svgH };

  // 计算每个节点坐标
  const pos = new Map<string, { x: number; y: number; w: number }>();
  for (const [l, nodeIds] of layerMap) {
    const x = layerX[l];
    const w = layerWidths.get(l) ?? 72;
    const totalH = nodeIds.length * NODE_H + (nodeIds.length - 1) * V_GAP;
    const startY = PAD + (svgH - PAD * 2 - totalH) / 2;
    nodeIds.forEach((id, i) => pos.set(id, { x, y: startY + i * (NODE_H + V_GAP), w }));
  }

  const latColor = (us: number) => {
    const ms = us / 1000;
    return ms < 80 ? "#059669" : ms < 200 ? "#d97706" : "#e11d48";
  };

  const allOk = segments.every((s) => s.ok);
  const byTo = new Map<string, number>();
  for (const s of segments) {
    if (!s.ok || !s.to) continue;
    byTo.set(s.to, Math.max(byTo.get(s.to) ?? 0, s.latency_us));
  }
  const total = Array.from(byTo.values()).reduce((a, b) => a + b, 0);
  const totalColor = total / 1000 < 80 ? "#059669" : total / 1000 < 200 ? "#d97706" : "#e11d48";

  // 适应容器
  function fitToContainer() {
    if (!containerRef.current) return;
    const rect = containerRef.current.getBoundingClientRect();
    const cw = rect.width;
    const ch = rect.height;
    const fit = Math.min(cw / svgW, ch / svgH);
    const s = clamp(fit >= 1 ? 1 : fit * 0.9, 0.15, 1);
    setT({
      scale: s,
      tx: (cw - svgW * s) / 2,
      ty: (ch - svgH * s) / 2,
    });
  }

  // 滚轮缩放（向光标位置缩放）
  function handleWheel(e: React.WheelEvent<HTMLDivElement>) {
    e.preventDefault();
    if (!containerRef.current) return;
    const rect = containerRef.current.getBoundingClientRect();
    const mx = e.clientX - rect.left;
    const my = e.clientY - rect.top;
    setT((prev) => {
      const factor = e.deltaY < 0 ? 1.15 : 1 / 1.15;
      const ns = clamp(prev.scale * factor, 0.15, 4);
      return {
        scale: ns,
        tx: mx - (mx - prev.tx) * (ns / prev.scale),
        ty: my - (my - prev.ty) * (ns / prev.scale),
      };
    });
  }

  // 拖拽开始
  function handleMouseDown(e: React.MouseEvent<HTMLDivElement>) {
    if (e.button !== 0) return;
    dragRef.current = {
      ox: e.clientX,
      oy: e.clientY,
      otx: t.tx,
      oty: t.ty,
    };
  }

  // 拖拽移动
  function handleMouseMove(e: React.MouseEvent<HTMLDivElement>) {
    if (!dragRef.current) return;
    const { ox, oy, otx, oty } = dragRef.current;
    setT((prev) => ({
      ...prev,
      tx: otx + (e.clientX - ox),
      ty: oty + (e.clientY - oy),
    }));
  }

  // 拖拽结束
  function handleMouseUp() {
    dragRef.current = null;
  }

  // 控制按钮缩放（以容器中心为基准）
  function zoomBy(factor: number) {
    if (!containerRef.current) return;
    const rect = containerRef.current.getBoundingClientRect();
    const mx = rect.width / 2;
    const my = rect.height / 2;
    setT((prev) => {
      const ns = clamp(prev.scale * factor, 0.15, 4);
      return {
        scale: ns,
        tx: mx - (mx - prev.tx) * (ns / prev.scale),
        ty: my - (my - prev.ty) * (ns / prev.scale),
      };
    });
  }

  return (
    <div className="space-y-2">
      {/* 控制栏 */}
      <div className="flex items-center justify-end gap-1">
        <button
          type="button"
          onClick={() => zoomBy(1.2)}
          className="rounded border px-1.5 py-0.5 text-xs hover:bg-muted"
          title="放大"
        >
          ＋
        </button>
        <button
          type="button"
          onClick={() => zoomBy(1 / 1.2)}
          className="rounded border px-1.5 py-0.5 text-xs hover:bg-muted"
          title="缩小"
        >
          －
        </button>
        <button
          type="button"
          onClick={fitToContainer}
          className="rounded border px-1.5 py-0.5 text-xs hover:bg-muted"
          title="适应窗口"
        >
          适应
        </button>
        <span className="w-10 text-right font-mono text-xs text-muted-foreground">
          {Math.round(t.scale * 100)}%
        </span>
      </div>

      {/* 可平移/缩放画布容器 */}
      <div
        ref={containerRef}
        style={{ height: 300, overflow: "hidden", cursor: dragRef.current ? "grabbing" : "grab" }}
        onWheel={handleWheel}
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onMouseUp={handleMouseUp}
        onMouseLeave={handleMouseUp}
      >
        <div
          style={{
            transform: `translate(${t.tx}px, ${t.ty}px) scale(${t.scale})`,
            transformOrigin: "0 0",
            willChange: "transform",
          }}
        >
          <svg width={svgW} height={svgH} style={{ overflow: "visible" }}>
            {/* 边 */}
            {edges.map((e, i) => {
              const f = pos.get(e.from);
              const tgt = pos.get(e.to);
              if (!f || !tgt) return null;
              const x1 = f.x + f.w;
              const y1 = f.y + NODE_H / 2;
              const x2 = tgt.x;
              const y2 = tgt.y + NODE_H / 2;
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

            {/* 节点 */}
            {[...pos.entries()].map(([id, { x, y, w }]) => (
              <g key={id}>
                <rect
                  x={x} y={y} width={w} height={NODE_H} rx={6}
                  fill="hsl(var(--muted))" stroke="hsl(var(--border))" strokeWidth={1}
                />
                <text
                  x={x + w / 2} y={y + NODE_H / 2 + 4}
                  textAnchor="middle" fontSize={11} fontWeight={500}
                  fill="hsl(var(--foreground))"
                >
                  {labels.get(id) ?? id}
                </text>
              </g>
            ))}
          </svg>
        </div>
      </div>

      {/* 合计延迟 */}
      {allOk && segments.length > 0 && (
        <div className="flex items-center justify-between border-t pt-2 text-sm">
          <span className="text-muted-foreground">合计</span>
          <span className="font-mono tabular-nums font-semibold" style={{ color: totalColor }}>
            {(total / 1000).toFixed(1)} ms
          </span>
        </div>
      )}

      {/* 错误信息 */}
      {segments.some((s) => !s.ok) && (
        <div className="rounded-md border border-rose-200 bg-rose-50/60 p-2 text-xs text-rose-700 dark:border-rose-900/40 dark:bg-rose-950/30 dark:text-rose-400">
          {segments.filter((s) => !s.ok).map((s, i) => (
            <div key={i} className="font-mono">{s.error}</div>
          ))}
        </div>
      )}

      {/* 流式探测进行中提示 */}
      {streaming && segments.length > 0 && (
        <div className="flex items-center gap-2 pt-1 text-xs text-muted-foreground">
          <span className="h-3 w-3 rounded-full border-2 border-primary border-t-transparent animate-spin" />
          探测中…
        </div>
      )}
    </div>
  );
}
