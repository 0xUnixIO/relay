import { BrowserRouter, Routes, Route, Navigate } from "react-router-dom";
import { Toaster } from "sonner";
import Layout from "@/components/Layout";
import ProtectedRoute from "@/components/ProtectedRoute";
import RequireAdmin from "@/components/RequireAdmin";
import Login from "@/pages/Login";
import Dashboard from "@/pages/Dashboard";
import NodesPage from "@/pages/Nodes";
import NodeDetail from "@/pages/NodeDetail";
import ForwardsPage from "@/pages/Forwards";
import TunnelsPage from "@/pages/Tunnels";
import UsersPage from "@/pages/Users";
import UserGroupsPage from "@/pages/UserGroups";
import MePage from "@/pages/Me";
import RoleHome from "@/components/RoleHome";
import UserDashboard from "@/pages/UserDashboard";
import ConfigPage from "@/pages/Config";
import ProbePage from "@/pages/Probe";
import NodeOverview from "@/pages/NodeOverview";
import { ConfirmProvider } from "@/hooks/useConfirm";
import { TooltipProvider } from "@/components/ui/tooltip";

export default function App() {
  return (
    <BrowserRouter>
      <TooltipProvider delayDuration={300}>
      <ConfirmProvider>
      <Toaster position="bottom-right" richColors />
      <Routes>
        <Route path="/login" element={<Login />} />
        <Route element={<ProtectedRoute />}>
          <Route element={<Layout />}>
            <Route index element={<RoleHome />} />
            <Route path="/me" element={<MePage />} />
            <Route path="/forwards" element={<ForwardsPage />} />
            <Route path="/probe" element={<ProbePage />} />
            <Route path="/user-home" element={<UserDashboard />} />
            <Route element={<RequireAdmin />}>
              <Route path="/dashboard" element={<Dashboard />} />
              <Route path="/nodes" element={<NodesPage />} />
              <Route path="/nodes/:id" element={<NodeDetail />} />
              <Route path="/tunnels" element={<TunnelsPage />} />
              <Route path="/users" element={<UsersPage />} />
              <Route path="/user-groups" element={<UserGroupsPage />} />
              <Route path="/config" element={<ConfigPage />} />
              <Route path="/overview" element={<NodeOverview />} />
            </Route>
            <Route path="*" element={<Navigate to="/" replace />} />
          </Route>
        </Route>
      </Routes>
      </ConfirmProvider>
      </TooltipProvider>
    </BrowserRouter>
  );
}
