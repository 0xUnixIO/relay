import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);

// React 第一帧渲染完（BootSplash 已可见）后移除原生 pre-splash
requestAnimationFrame(() => {
  document.getElementById("pre-splash")?.remove();
});
