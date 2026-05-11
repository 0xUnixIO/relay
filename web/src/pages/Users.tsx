import { useState, useMemo } from "react";
import useSWR from "swr";
import { Plus, Pencil, Trash2, Shuffle, Users as UsersIcon, CalendarIcon, X } from "lucide-react";
import { useConfirm } from "@/hooks/useConfirm";
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
  Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle,
} from "@/components/ui/dialog";
import {
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from "@/components/ui/select";
import { Api, type User } from "@/lib/api";
import { toast } from "sonner";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { Calendar } from "@/components/ui/calendar";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";


type UserForm = {
  username: string;
  password: string;
  role: string;
  status: string;
  expires_at: string;
  remark: string;
};

const emptyUserForm = (): UserForm => ({
  username: "",
  password: "",
  role: "user",
  status: "active",
  expires_at: "",
  remark: "",
});

export default function UsersPage() {
  const confirm = useConfirm();
  const { data: users = [], mutate } = useSWR("users", Api.listUsers);
  const { data: forwards = [] } = useSWR("forwards", Api.listForwards);
  // @ts-ignore – reserved for future active-connection column
  const connsByUser = useMemo(() => {
    const m = new Map<string, number>();
    for (const f of forwards) {
      if (f.user_id) m.set(f.user_id, (m.get(f.user_id) ?? 0) + f.active_connections);
    }
    return m;
  }, [forwards]);

  const [open, setOpen] = useState(false);
  const [calOpen, setCalOpen] = useState(false);
  const [editing, setEditing] = useState<User | null>(null);
  const [form, setForm] = useState<UserForm>(emptyUserForm());
  const [saving, setSaving] = useState(false);
  // "" = 加载中, "__none__" = 无套餐, 其他 = groupId
  const [groupId, setGroupId] = useState("");
  const [origGroupId, setOrigGroupId] = useState("");

  const { data: groups = [] } = useSWR("user-groups", Api.listUserGroups);

  const openNew = () => {
    setEditing(null);
    setForm(emptyUserForm());
    setGroupId("__none__");
    setOrigGroupId("__none__");
    setOpen(true);
  };
  const openEdit = async (u: User) => {
    setEditing(u);
    setForm({
      username: u.username,
      password: "",
      role: u.role,
      status: u.status,
      expires_at: u.expires_at ? u.expires_at.slice(0, 10) : "",
      remark: u.remark ?? "",
    });
    setGroupId("");
    setOpen(true);
    let gid = "__none__";
    for (const g of await Api.listUserGroups()) {
      const members = await Api.listGroupMembers(g.id);
      if (members.some((m) => m.user_id === u.id)) { gid = g.id; break; }
    }
    setGroupId(gid);
    setOrigGroupId(gid);
  };

  const submit = async () => {
    setSaving(true);
    try {
      const expires = form.expires_at
        ? new Date(`${form.expires_at}T23:59:59Z`).toISOString()
        : null;
      if (editing) {
        await Api.updateUser(editing.id, {
          role: form.role,
          status: form.status,
          expires_at: expires,
          remark: form.remark,
          ...(form.password ? { password: form.password } : {}),
        });
        if (groupId !== "" && groupId !== origGroupId) {
          if (origGroupId !== "__none__") await Api.removeGroupMember(origGroupId, editing.id);
          if (groupId !== "__none__") {
            await Api.addGroupMember(groupId, editing.id);
            await Api.applyGroupTunnels(groupId);
          }
        }
      } else {
        await Api.createUser({
          username: form.username,
          password: form.password,
          role: form.role,
          status: form.status,
          expires_at: expires,
          remark: form.remark,
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

  const remove = async (u: User) => {
    if (!await confirm(`确定删除用户 ${u.username}？关联的隧道分配与转发将一并删除。`)) return;
    try {
      await Api.deleteUser(u.id);
      mutate();
    } catch (e: any) {
      toast.error(e?.message ?? "删除失败");
    }
  };

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold">用户</h1>
        <Button onClick={openNew}>
          <Plus className="mr-1 h-4 w-4" /> 新建用户
        </Button>
      </div>

      <Card>
        <CardContent className="p-0">
          <ScrollArea>
          <Table className="min-w-[860px] table-fixed">
            <colgroup>
              <col style={{ width: "8rem" }} />
              <col style={{ width: "5rem" }} />
              <col style={{ width: "5rem" }} />
              <col style={{ width: "7rem" }} />
              <col style={{ width: "7rem" }} />
              <col style={{ width: "7rem" }} />
              <col />
              <col style={{ width: "7rem" }} />
            </colgroup>
            <TableHeader>
              <TableRow>
                <TableHead>用户名</TableHead>
                <TableHead>角色</TableHead>
                <TableHead>状态</TableHead>
                <TableHead>到期</TableHead>
                <TableHead>套餐</TableHead>
                <TableHead>活跃连接</TableHead>
                <TableHead>备注</TableHead>
                <TableHead>操作</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {users.length === 0 ? (
                <TableRow className="hover:bg-transparent even:bg-transparent">
                  <TableCell colSpan={8} className="text-center">
                    <EmptyState icon={UsersIcon} title="暂无用户" description="点击右上角「新建用户」按钮创建。" compact />
                  </TableCell>
                </TableRow>
              ) : (
                users.map((u) => (
                  <TableRow key={u.id}>
                    <TableCell className="font-medium truncate" title={u.username}>{u.username}</TableCell>
                    <TableCell>
                      <Badge variant="secondary">{u.role}</Badge>
                    </TableCell>
                    <TableCell>
                      {u.status === "active" ? (
                        <Badge variant="success">启用</Badge>
                      ) : (
                        <Badge variant="outline">{u.status}</Badge>
                      )}
                    </TableCell>
                    <TableCell className="text-xs whitespace-nowrap">
                      {u.expires_at ? u.expires_at.slice(0, 10) : "—"}
                    </TableCell>
                    <TableCell className="text-xs text-muted-foreground truncate">
                      {u.group_name ?? "—"}
                    </TableCell>
                    <TableCell className="text-xs tabular-nums text-muted-foreground">
                      {connsByUser.get(u.id) ?? 0}
                    </TableCell>
                    <TableCell className="text-xs text-muted-foreground truncate" title={u.remark || ""}>{u.remark || "—"}</TableCell>
                    <TableCell className="space-x-1 whitespace-nowrap">
                      <Button size="icon" variant="ghost" onClick={() => openEdit(u)} title="编辑">
                        <Pencil className="h-4 w-4" />
                      </Button>
                      <Button size="icon" variant="ghost" onClick={() => remove(u)} title="删除">
                        <Trash2 className="h-4 w-4" />
                      </Button>
                    </TableCell>
                  </TableRow>
                ))
              )}
            </TableBody>
          </Table>
          </ScrollArea>
        </CardContent>
      </Card>

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>{editing ? "编辑用户" : "新建用户"}</DialogTitle>
          </DialogHeader>
          <div className="space-y-3">
            <div className="space-y-1.5">
              <Label>用户名</Label>
              <Input
                disabled={!!editing}
                value={form.username}
                onChange={(e) => setForm({ ...form, username: e.target.value })}
              />
            </div>
            <div className="space-y-1.5">
              <Label>{editing ? "新密码（留空保持不变）" : "密码"}</Label>
              <div className="flex gap-2">
                <Input
                  type="text"
                  value={form.password}
                  onChange={(e) => setForm({ ...form, password: e.target.value })}
                  className="font-mono flex-1"
                />
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Button
                      type="button"
                      variant="outline"
                      size="icon"
                      onClick={() => {
                        const chars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!@#$%^&*";
                        const pwd = Array.from(crypto.getRandomValues(new Uint8Array(16)))
                          .map((b) => chars[b % chars.length])
                          .join("");
                        setForm({ ...form, password: pwd });
                      }}
                    >
                      <Shuffle className="h-4 w-4" />
                    </Button>
                  </TooltipTrigger>
                  <TooltipContent>随机生成密码</TooltipContent>
                </Tooltip>
              </div>
            </div>
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
              <div className="space-y-1.5">
                <Label>角色</Label>
                <Select
                  value={form.role}
                  onValueChange={(v) => setForm({ ...form, role: v })}
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="user">user</SelectItem>
                    <SelectItem value="admin">admin</SelectItem>
                  </SelectContent>
                </Select>
              </div>
              <div className="space-y-1.5">
                <Label>状态</Label>
                <Select
                  value={form.status}
                  onValueChange={(v) => setForm({ ...form, status: v })}
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="active">active</SelectItem>
                    <SelectItem value="disabled">disabled</SelectItem>
                  </SelectContent>
                </Select>
              </div>
            </div>
            <div className="space-y-1.5">
              <Label>到期日期（可选）</Label>
              <Popover open={calOpen} onOpenChange={setCalOpen}>
                <PopoverTrigger asChild>
                  <Button
                    variant="outline"
                    className={cn("w-full justify-start text-left font-normal", !form.expires_at && "text-muted-foreground")}
                  >
                    <CalendarIcon className="mr-2 h-4 w-4" />
                    {form.expires_at || "选择日期"}
                    {form.expires_at && (
                      <X
                        className="ml-auto h-4 w-4 opacity-50 hover:opacity-100"
                        onClick={(e) => { e.stopPropagation(); setForm({ ...form, expires_at: "" }); }}
                      />
                    )}
                  </Button>
                </PopoverTrigger>
                <PopoverContent className="w-auto p-0" align="start">
                  <Calendar
                    mode="single"
                    captionLayout="dropdown"
                    fromYear={new Date().getFullYear()}
                    toYear={new Date().getFullYear() + 15}
                    selected={form.expires_at ? new Date(form.expires_at) : undefined}
                    onSelect={(date) => {
                      setForm({ ...form, expires_at: date ? date.toLocaleDateString("sv") : "" });
                      setCalOpen(false);
                    }}
                    disabled={(date) => date < new Date(new Date().setHours(0, 0, 0, 0))}
                  />
                </PopoverContent>
              </Popover>
            </div>
            <div className="space-y-1.5">
              <Label>备注</Label>
              <Input
                value={form.remark}
                onChange={(e) => setForm({ ...form, remark: e.target.value })}
              />
            </div>
            {editing && (
              <div className="space-y-1.5">
                <Label>套餐</Label>
                <Select value={groupId} onValueChange={setGroupId} disabled={groupId === ""}>
                  <SelectTrigger>
                    <SelectValue placeholder="加载中…" />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="__none__">无</SelectItem>
                    {groups.map((g) => (
                      <SelectItem key={g.id} value={g.id}>{g.name}</SelectItem>
                    ))}
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

    </div>
  );
}

