import { Link, NavLink, Outlet, useLocation, useNavigate } from "react-router-dom";
import { useEffect, useMemo, useRef, useState } from "react";
import React from "react";
import { LayoutDashboard, Server, Network, Route as RouteIcon, LogOut, Github, Sun, Moon, Users, Package, User as UserIcon, Settings, Menu, ArrowUpCircle, Loader2, Activity } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { clearAuth, getUser, getRole } from "@/lib/auth";
import { cn } from "@/lib/utils";
import { Api, ApiError } from "@/lib/api";
import { useTheme } from "@/lib/theme";
import { toast } from "sonner";
import AreaSwitcher, { type Area } from "@/components/AreaSwitcher";
import BootSplash from "@/components/BootSplash";

function RelayLogo({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 32 32"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      className={className}
      aria-hidden
    >
      <defs>
        <linearGradient id="relay-r" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="hsl(199 78% 56%)" />
          <stop offset="100%" stopColor="hsl(199 78% 38%)" />
        </linearGradient>
      </defs>
      {/* Filled letter "R" — single path, even-odd fill. Outer outline traces
          stem → bowl → leg; inner subpath cuts out the hole inside the bowl. */}
      <path
        fill="url(#relay-r)"
        fillRule="evenodd"
        d="M6 4 V28 H10 V18 H13 L21 28 H26 L17 17.5 Q22 16 22 11 Q22 4 14 4 Z M10 8 H14 Q18 8 18 11 Q18 14 14 14 H10 Z"
      />
    </svg>
  );
}

interface NavItem { to: string; label: string; icon: React.ComponentType<{ className?: string }>; end?: boolean }
interface NavGroup { group: string | null; items: NavItem[] }

const adminAreaNav: NavGroup[] = [
  { group: null, items: [
    { to: "/", label: "仪表", icon: LayoutDashboard, end: true },
    { to: "/nodes", label: "节点", icon: Server },
    { to: "/tunnels", label: "隧道", icon: RouteIcon },
    { to: "/users", label: "用户", icon: Users },
    { to: "/user-groups", label: "套餐", icon: Package },
    { to: "/config", label: "配置", icon: Settings },
  ]},
];

const userAreaNav: NavGroup[] = [
  { group: null, items: [
    { to: "/user-home", label: "首页", icon: LayoutDashboard },
    { to: "/forwards", label: "转发", icon: Network },
    { to: "/probe", label: "监控", icon: Activity },
    { to: "/me", label: "账户", icon: UserIcon },
  ]},
];

// Non-admin viewers see a slightly different default landing path mapping.
const userOnlyNav: NavGroup[] = [
  { group: null, items: [
    { to: "/", label: "首页", icon: LayoutDashboard, end: true },
    { to: "/forwards", label: "转发", icon: Network },
    { to: "/probe", label: "监控", icon: Activity },
    { to: "/me", label: "账户", icon: UserIcon },
  ]},
];

const USER_AREA_PATHS = ["/user-home", "/forwards", "/me", "/probe"];
const AREA_STORAGE_KEY = "relay.area";

function deriveArea(pathname: string, fallback: Area): Area {
  if (USER_AREA_PATHS.some((p) => pathname === p || pathname.startsWith(p + "/"))) {
    return "user";
  }
  // Treat all other admin-only paths as admin area; ambiguous root "/" keeps fallback.
  if (pathname === "/") return fallback;
  return "admin";
}

export default function Layout() {
  const navigate = useNavigate();
  const location = useLocation();
  const user = getUser();
  const role = getRole();
  const { theme, toggle } = useTheme();
  const [masterVersion, setMasterVersion] = useState<string | null>(null);
  const [latestVersion, setLatestVersion] = useState<string | null>(null);
  const [brandName, setBrandName] = useState<string>("RELAY");
  const [bootReady, setBootReady] = useState(false);
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [upgradeOpen, setUpgradeOpen] = useState(false);
  const [selfUpgrading, setSelfUpgrading] = useState(false);
  const upgradePollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const isAdmin = role === "admin";

  // Persisted area preference (admin only). Auto-syncs to current path so
  // refresh / direct links land on the matching area.
  const [area, setAreaState] = useState<Area>(() => {
    if (!isAdmin) return "user";
    const stored = (typeof localStorage !== "undefined" && localStorage.getItem(AREA_STORAGE_KEY)) as Area | null;
    return deriveArea(location.pathname, stored === "user" ? "user" : "admin");
  });

  // Keep area in sync when the user navigates via links / back button.
  useEffect(() => {
    if (!isAdmin) return;
    const next = deriveArea(location.pathname, area);
    if (next !== area) setAreaState(next);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [location.pathname, isAdmin]);

  const setArea = (next: Area) => {
    setAreaState(next);
    try { localStorage.setItem(AREA_STORAGE_KEY, next); } catch { /* ignore */ }
    navigate(next === "admin" ? "/" : "/user-home");
  };

  const nav = useMemo<NavGroup[]>(() => {
    if (!isAdmin) return userOnlyNav;
    return area === "admin" ? adminAreaNav : userAreaNav;
  }, [isAdmin, area]);

  useEffect(() => {
    // Bootstrap: fetch the operator-configured brand + master version before
    // showing the layout, so users never see a flash of default values
    // (e.g. "RELAY") that gets replaced a beat later.
    let cancelled = false;
    Promise.allSettled([Api.serverInfo(), Api.getBranding(), Api.getSystemVersion()]).then((results) => {
      if (cancelled) return;
      const info = results[0];
      const brand = results[1];
      const sysVer = results[2];
      if (info.status === "fulfilled") setMasterVersion(info.value.version);
      if (brand.status === "fulfilled") {
        setBrandName(brand.value.brand_name || "RELAY");
      }
      if (sysVer.status === "fulfilled") {
        const tag = sysVer.value.latest_stable?.tag ?? null;
        setLatestVersion(tag ? tag.replace(/^v/, "") : null);
      }
      setBootReady(true);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const logout = () => {
    clearAuth();
    navigate("/login");
  };

  const doSelfUpgrade = async () => {
    setSelfUpgrading(true);
    try {
      await Api.selfUpgradeMaster(`v${latestVersion!}`);
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e));
      setSelfUpgrading(false);
      return;
    }
    setTimeout(() => {
      upgradePollRef.current = setInterval(async () => {
        try {
          const r = await fetch("/health", { cache: "no-store" });
          if (r.ok) {
            clearInterval(upgradePollRef.current!);
            upgradePollRef.current = null;
            window.location.reload();
          }
        } catch { /* 还在重启 */ }
      }, 2000);
    }, 15_000);
  };

  return (
    <div className="flex h-screen overflow-hidden bg-background text-foreground">
      {!bootReady && <BootSplash />}
      {/* AreaSwitcher：桌面端 fixed 浮动居中；移动端移入 header 内部，避免 fixed 层遮挡汉堡/退出按钮 */}
      {isAdmin && (
        <div className="pointer-events-none fixed inset-x-0 top-0 z-[60] hidden md:flex h-14 items-center justify-center">
          <div className="pointer-events-auto -translate-y-[3px] drop-shadow-[0_6px_14px_rgba(0,0,0,0.18)] dark:drop-shadow-[0_6px_18px_rgba(0,0,0,0.55)]">
            <AreaSwitcher value={area} onChange={setArea} />
          </div>
        </div>
      )}
      {/* Mobile overlay */}
      {drawerOpen && (
        <div
          className="fixed inset-0 z-40 bg-black/60 md:hidden"
          onClick={() => setDrawerOpen(false)}
        />
      )}

      {/* Sidebar — slide-in drawer on mobile, static on desktop */}
      <aside
        className={cn(
          "fixed inset-y-0 left-0 z-50 flex w-56 flex-col border-r bg-white dark:bg-[#111111] dark:border-white/[0.06] transition-transform duration-200",
          "md:static md:translate-x-0",
          drawerOpen ? "translate-x-0" : "-translate-x-full",
        )}
      >
        <Link
          to="/"
          onClick={() => setDrawerOpen(false)}
          className="flex h-14 items-center gap-2.5 border-b px-4 font-bold uppercase tracking-[0.2em] text-foreground dark:border-white/[0.06]"
          title={brandName}
        >
          <RelayLogo className="h-6 w-6 shrink-0 drop-shadow-[0_2px_5px_hsl(199_78%_44%/0.35)]" />
          <span className="truncate">{brandName}</span>
        </Link>
        <nav className="flex-1 p-2.5 space-y-3 overflow-y-auto">
          {nav.map((group, gi) => (
            <div key={gi} className="space-y-0.5">
              {group.group && (
                <div className="px-3 pb-1 pt-0.5 text-sm font-semibold uppercase tracking-wider text-muted-foreground/70 select-none">
                  {group.group}
                </div>
              )}
              {group.items.map((n) => (
                <NavLink
                  key={n.to}
                  to={n.to}
                  end={n.end}
                  onClick={() => setDrawerOpen(false)}
                  className={({ isActive }) =>
                    cn(
                      "flex items-center gap-2.5 rounded-lg px-3 py-2 text-base transition-colors",
                      isActive
                        ? "bg-foreground text-background font-medium"
                        : "text-muted-foreground hover:bg-accent hover:text-foreground",
                    )
                  }
                >
                  <n.icon className="h-4 w-4" />
                  {n.label}
                </NavLink>
              ))}
            </div>
          ))}
        </nav>
        <div className="border-t p-3 flex items-center justify-between dark:border-white/[0.06]">
          <a
            href="https://github.com/0xUnixIO/relay"
            target="_blank"
            rel="noopener noreferrer"
            className="flex items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground transition-colors"
          >
            <Github className="h-3.5 w-3.5" />
            0xUnixIO/relay
          </a>
          {masterVersion && (() => {
            const hasUpdate = latestVersion && latestVersion !== masterVersion;
            return hasUpdate ? (
              <button
                onClick={() => setUpgradeOpen(true)}
                className="relative text-[11px] text-muted-foreground hover:text-foreground transition-colors cursor-pointer"
                title={`新版本可用：v${latestVersion}，点击升级`}
              >
                v{masterVersion}
                <span className="absolute -top-0.5 -right-1.5 h-1.5 w-1.5 rounded-full bg-red-500" />
              </button>
            ) : (
              <span className="relative text-[11px] text-muted-foreground">
                v{masterVersion}
              </span>
            );
          })()}
        </div>
      </aside>

      {/* Master 自升级弹窗 */}
      <Dialog open={upgradeOpen} onOpenChange={(o) => { if (!selfUpgrading) setUpgradeOpen(o); }}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>发现新版本</DialogTitle>
            <DialogDescription>
              升级期间服务将短暂中断（约 15–30 秒），完成后页面自动刷新。
            </DialogDescription>
          </DialogHeader>
          <div className="flex items-center gap-3 py-2">
            <Badge variant="outline">当前 v{masterVersion}</Badge>
            <span className="text-muted-foreground text-sm">→</span>
            <Badge variant="success">v{latestVersion}</Badge>
          </div>
          {selfUpgrading && (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Loader2 className="h-4 w-4 animate-spin" />
              正在升级，请稍候…
            </div>
          )}
          <DialogFooter>
            <Button variant="outline" onClick={() => setUpgradeOpen(false)} disabled={selfUpgrading}>
              取消
            </Button>
            <Button onClick={doSelfUpgrade} disabled={selfUpgrading}>
              {selfUpgrading
                ? <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                : <ArrowUpCircle className="mr-2 h-4 w-4" />}
              立即升级
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <div className="flex flex-1 flex-col min-w-0">
        <header className="relative flex h-14 items-center justify-between border-b px-4 md:px-6 bg-background/80 backdrop-blur-sm">
          <div className="flex items-center gap-3">
            {/* Hamburger — mobile only */}
            <Button
              variant="ghost"
              size="icon"
              className="md:hidden"
              onClick={() => setDrawerOpen(true)}
              aria-label="打开菜单"
            >
              <Menu className="h-5 w-5" />
            </Button>
          </div>
          {/* 移动端 AreaSwitcher：绝对居中，不占用 flex 流，左右两侧仍可点击 */}
          {isAdmin && (
            <div className="pointer-events-none absolute inset-0 flex items-center justify-center md:hidden">
              <div className="pointer-events-auto drop-shadow-[0_4px_10px_rgba(0,0,0,0.15)] dark:drop-shadow-[0_4px_12px_rgba(0,0,0,0.45)]">
                <AreaSwitcher value={area} onChange={setArea} />
              </div>
            </div>
          )}
          <div className="flex items-center gap-2 text-sm">
            <span className="text-muted-foreground hidden sm:block">{user ?? "—"}</span>
            <Button variant="ghost" size="icon" onClick={(e) => toggle(e)} aria-label="切换主题">
              {theme === "dark" ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
            </Button>
            <Button variant="ghost" size="sm" onClick={logout}>
              <LogOut className="mr-1 h-4 w-4" />
              <span className="hidden sm:inline">退出</span>
            </Button>
          </div>
        </header>
        <main className="flex-1 overflow-auto p-4 md:p-6">
          <Outlet />
        </main>
      </div>
    </div>
  );
}
