import { defineConfig, type ConfigEnv } from "vite";
import vue from "@vitejs/plugin-vue";

// Sub-path support.
//
// Two ways to serve the whole SPA (assets + API calls) from a URL sub-directory
// such as https://host/qd:
//
//   1. Default (runtime-adaptive, recommended): the UI is built with a RELATIVE
//      base, so asset URLs in index.html resolve against whatever directory the
//      page is currently served under. API calls derive their prefix from the
//      current URL at runtime (see src/api.ts). The SAME image therefore works
//      at the bare root `/` OR any reverse-proxied sub-directory `/qd`, `/foo`
//      … without rebuilding.
//
//      Reverse-proxy that does NOT strip the prefix:
//        location /qd/ { proxy_pass http://127.0.0.1:8923; }   # no trailing /
//
//   2. Explicit build-time prefix (legacy): build with `VITE_BASE_PATH=/qd npm
//      run build` for an absolute base of `/qd/`. Kept for compatibility with
//      setups that prefer pinning the prefix at build time (the backend must
//      then be started with the matching QDRUST_BASE_PATH=/qd).
//
// During `vite dev` the app and the backend normally share the host; proxy the
// API + probes to the running qdrust-server. When an explicit build-time
// sub-path is configured, prefix the proxied keys so base-relative API calls
// resolve in dev too (start the backend with the matching QDRUST_BASE_PATH).
const apiTarget = "http://127.0.0.1:8923";
const basePath = (process.env.VITE_BASE_PATH || "").trim();

export default defineConfig((env: ConfigEnv) => {
  // Build: use a relative base by default so one image fits any prefix.
  // `vite dev` cannot reliably use a relative base, so it stays at "/" unless
  // an explicit VITE_BASE_PATH was requested.
  let base = "/";
  if (basePath) {
    base = `${basePath.replace(/\/+$/, "")}/`; // legacy absolute prefix
  } else if (env.command === "build") {
    base = "./"; // runtime-adaptive
  }

  const proxyPrefix = base === "/" || base === "./" ? "" : base.replace(/\/+$/, "");
  const makeProxy = (key: string) => ({ [key]: apiTarget });

  return {
    base,
    plugins: [vue()],
    server: {
      host: "127.0.0.1",
      port: 5173,
      proxy: {
        ...(proxyPrefix === ""
          ? {
              ...makeProxy("/api"),
              ...makeProxy("/health"),
              ...makeProxy("/ready")
            }
          : {
              [`${proxyPrefix}/api`]: apiTarget,
              [`${proxyPrefix}/health`]: apiTarget,
              [`${proxyPrefix}/ready`]: apiTarget
            })
      }
    }
  };
});
